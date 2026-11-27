use crate::guard::models::ValidateVerdict;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::timeout;
use tracing::{info, warn};

/// The SHA-256 digest of the multi-arch `alpine:3.21` OCI index, resolved
/// from the Docker Hub registry (`Docker-Content-Digest` on the manifest
/// `HEAD`) and cross-checked against `podman pull alpine:3.21`'s
/// `RepoDigests` (via `podman inspect`) on 2026-11-25. `SANDBOX_IMAGE`
/// composes the digest into the `alpine@sha256:<digest>` reference below;
/// the two must stay in sync (the
/// `sandbox_image_is_digest_pinned_and_never_latest` test enforces it).
// `pub` in a binary crate is not externally reachable, so non-test builds
// would otherwise flag this digest-only constant as dead code.
#[allow(dead_code)]
pub const SANDBOX_IMAGE_DIGEST: &str =
    "bc21487101ae47172509130ee1092cc273a831968d64b617776d1f69c81e42ab";

/// R26/issue #410: the sandbox image is pinned by digest rather than tag, so
/// a registry re-push of the tag cannot silently change what the guard
/// executes. Bump the digest deliberately (and update
/// `docs/features-api/ops/security.md`) after re-running the evaluator tests
/// against the new image.
pub const SANDBOX_IMAGE: &str = concat!(
    "alpine@sha256:",
    "bc21487101ae47172509130ee1092cc273a831968d64b617776d1f69c81e42ab"
);

/// How long a sandboxed evaluation may run before the container is killed.
const SANDBOX_TIMEOUT: Duration = Duration::from_secs(5);

/// Process-wide ceiling on simultaneous sandbox runs. Each container can
/// reserve 512 MiB and 256 pids; excess requests queue on this semaphore
/// instead of exhausting the host or the container engine.
pub(crate) const SANDBOX_MAX_CONCURRENT_RUNS: usize = 8;

/// A queued request may not hold its payload past this deadline, and at most
/// this many requests may queue at once. Without both bounds, callers wait
/// beyond the advertised run timeout while the eight permits stay busy.
pub(crate) const SANDBOX_QUEUE_TIMEOUT: Duration = Duration::from_secs(2);
pub(crate) const SANDBOX_MAX_QUEUED: usize = 16;

/// Per-run container memory reservation, matching `--memory=512m` below.
const SANDBOX_PER_RUN_MEMORY_BYTES: u64 = 512 * 1024 * 1024;
/// Default aggregate budget. Operators can raise it (implicitly raising
/// concurrency) with `NEUROSTRATA_SANDBOX_MEMORY_BUDGET_MB`; the hard cap of
/// [`SANDBOX_MAX_CONCURRENT_RUNS`] still applies.
const SANDBOX_TOTAL_MEMORY_BUDGET_BYTES: u64 = 1024 * 1024 * 1024;
/// Writable scratch is a bounded tmpfs, never the container's root overlay.
const SANDBOX_TMPFS_SIZE: &str = "64m";

static SANDBOX_CONCURRENCY: std::sync::OnceLock<tokio::sync::Semaphore> =
    std::sync::OnceLock::new();
static SANDBOX_WAITERS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn sandbox_concurrency() -> &'static tokio::sync::Semaphore {
    SANDBOX_CONCURRENCY.get_or_init(|| tokio::sync::Semaphore::new(effective_sandbox_runs()))
}

/// Derive the live permit count from BOTH a configurable aggregate budget and
/// the host's available memory, reserving 20% for the daemon and other
/// processes. Eight 512 MiB containers can otherwise trigger a host-wide OOM
/// on a machine with less than 4 GiB free.
fn effective_sandbox_runs_for(available_bytes: u64, budget_bytes: u64, hard_cap: usize) -> usize {
    let budget_runs = (budget_bytes / SANDBOX_PER_RUN_MEMORY_BYTES) as usize;
    let usable = available_bytes.saturating_mul(8) / 10;
    let host_runs = (usable / SANDBOX_PER_RUN_MEMORY_BYTES) as usize;
    hard_cap.min(budget_runs).min(host_runs).max(1)
}

fn host_available_memory_bytes() -> Option<u64> {
    let info = std::fs::read_to_string("/proc/meminfo").ok()?;
    for line in info.lines() {
        if let Some(value) = line.strip_prefix("MemAvailable:") {
            let kb: u64 = value.split_whitespace().next()?.parse().ok()?;
            return Some(kb.saturating_mul(1024));
        }
    }
    None
}

fn effective_sandbox_runs() -> usize {
    let budget = std::env::var("NEUROSTRATA_SANDBOX_MEMORY_BUDGET_MB")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(|mb| mb.saturating_mul(1024 * 1024))
        .unwrap_or(SANDBOX_TOTAL_MEMORY_BUDGET_BYTES);
    // No host reading (non-Linux) means the configured budget alone governs.
    effective_sandbox_runs_for(
        host_available_memory_bytes().unwrap_or(u64::MAX),
        budget,
        SANDBOX_MAX_CONCURRENT_RUNS,
    )
}

struct QueueSlot;

impl Drop for QueueSlot {
    fn drop(&mut self) {
        SANDBOX_WAITERS.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Acquire one run permit, bounded both by queue depth and by wall time.
pub(crate) async fn acquire_sandbox_permit(
    limit: Duration,
) -> Result<tokio::sync::SemaphorePermit<'static>, String> {
    // Only callers that actually have to wait occupy a queue slot. Counting
    // immediate acquisitions rejects valid runs when several callers overlap
    // and a permit is already free.
    if let Ok(permit) = sandbox_concurrency().try_acquire() {
        return Ok(permit);
    }
    let waiting = SANDBOX_WAITERS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    let _slot = QueueSlot;
    if waiting > SANDBOX_MAX_QUEUED {
        return Err(format!(
            "sandbox queue full ({waiting} waiting, limit {SANDBOX_MAX_QUEUED})"
        ));
    }
    match timeout(limit, sandbox_concurrency().acquire()).await {
        Ok(Ok(permit)) => Ok(permit),
        Ok(Err(_)) => Err("sandbox concurrency semaphore closed".to_string()),
        Err(_) => Err(format!("sandbox queue wait exceeded {limit:?}")),
    }
}

/// Only shell payloads can be streamed into the sandbox. Normalize casing and
/// surrounding whitespace so `BASH` is sandboxed like `bash`; anything else
/// fails closed when the caller requested sandboxing instead of being approved
/// without any container or mount checks.
fn sandbox_action_type(action_type: &str) -> Result<&'static str, String> {
    match action_type.trim().to_ascii_lowercase().as_str() {
        "bash" => Ok("bash"),
        "script" => Ok("script"),
        _ => Err(format!(
            "Sandbox execution requested for unsupported action_type '{action_type}'; only bash and script can be sandboxed"
        )),
    }
}

/// Case variants of shell actions are sandboxed; non-shell actions never
/// mount anything, so they keep the documented `ApprovedFailOpen` verdict
/// after the deterministic and semantic rule checks instead of being
/// rejected for a container they were never going to use.
fn sandbox_action_decision(action_type: &str) -> Result<&'static str, ValidateVerdict> {
    match sandbox_action_type(action_type) {
        Ok(normalized) => Ok(normalized),
        Err(_) => Err(ValidateVerdict::ApprovedFailOpen {
            warning: format!(
                "Sandbox execution not applicable for action_type '{action_type}'; approved after deterministic and semantic rule checks"
            ),
        }),
    }
}

/// The caller's workspace is the daemon project root plus the operator's
/// `allowed_external_dirs`. A canonical cwd outside both must never be bind
/// mounted into the sandbox, even when it is readable and not on the
/// sensitive-path refusal list.
fn sandbox_allowed_external_dirs() -> Vec<PathBuf> {
    let Ok(path) = crate::config::Config::default_path() else {
        return Vec::new();
    };
    if !path.exists() {
        return Vec::new();
    }
    match crate::config::Config::load_from(&path) {
        Ok(config) => config.allowed_external_dirs,
        Err(e) => {
            warn!(
                "could not read {} for sandbox allowed_external_dirs; treating it as empty: {e}",
                path.display()
            );
            Vec::new()
        }
    }
}

fn workspace_cwd_refusal_with(cwd: &Path, root: &Path, allowlist: &[PathBuf]) -> Option<String> {
    crate::daemon::check_caller_path(cwd, root, allowlist).err()
}

fn workspace_cwd_refusal(cwd: &Path) -> Option<String> {
    workspace_cwd_refusal_with(
        cwd,
        &crate::daemon::resolve_project_root(),
        &sandbox_allowed_external_dirs(),
    )
}

/// Host paths a guarded action's working directory may never sit under: the
/// sandbox mounts the directory read-only at `/workspace`, so anything here
/// would be exposed to the evaluated payload. `/` (the filesystem root) is
/// handled separately because every absolute path starts with it.
const FORBIDDEN_WORKING_DIRECTORIES: &[&str] = &[
    "/etc",
    "/home",
    "/var/run",
    "/run",
    "/private/var/run",
    "/root",
    "/sys",
    "/dev",
    "/proc",
    "/boot",
];

/// Sensitive directories directly under a user's home. A checkout under the
/// home directory is a normal project; these are credentials and runtime
/// state and must never be mounted at `/workspace`.
const SENSITIVE_HOME_CHILDREN: &[&str] = &[
    ".ssh",
    ".aws",
    ".gnupg",
    ".kube",
    ".docker",
    ".config",
    ".password-store",
    ".local/share/keyrings",
    "Library/Keychains",
    "Library/Application Support",
];

/// R26: whether a Podman engine is reachable, from a synchronous probe. Callers
/// that cannot afford the async evaluator constructor use this to degrade
/// gracefully.
pub fn probe_podman_available() -> bool {
    std::process::Command::new("podman")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub struct SandboxEvaluator {
    /// ("podman" or "none", available), detected on the first `evaluate`.
    ///
    /// Detection spawns `podman --version` (~200 ms on Windows). It used to
    /// run in the constructor, and `neurostrata_guard_validate` builds a new
    /// validator per call, so every call paid for it although the hook calls
    /// with `use_sandbox: false` and never reaches `evaluate`.
    engine: tokio::sync::OnceCell<(String, bool)>,
    /// How many times `detect_engine` ran for this evaluator.
    engine_probes: std::sync::atomic::AtomicUsize,
}

impl Default for SandboxEvaluator {
    fn default() -> Self {
        Self::new()
    }
}

impl SandboxEvaluator {
    pub fn new() -> Self {
        Self {
            engine: tokio::sync::OnceCell::new(),
            engine_probes: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// How many times this evaluator ran engine detection (0 until the first
    /// `evaluate`, then 1).
    pub fn engine_probe_count(&self) -> usize {
        self.engine_probes
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    async fn detected_engine(&self) -> (String, bool) {
        self.engine
            .get_or_init(|| async {
                self.engine_probes
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Self::detect_engine().await
            })
            .await
            .clone()
    }

    async fn detect_engine() -> (String, bool) {
        // Prefer podman (per AGENTS.md: ALWAYS use podman, NEVER use docker)
        if let Ok(output) = Command::new("podman").arg("--version").output().await {
            if output.status.success() {
                return ("podman".to_string(), true);
            }
        }

        // No fallback to docker per project constraints
        ("none".to_string(), false)
    }

    pub async fn evaluate(
        &self,
        action_type: &str,
        payload: &str,
        cwd: &str,
    ) -> anyhow::Result<ValidateVerdict> {
        // Re-probe dynamically: `available` is computed once per evaluator
        // (the daemon's /validate route keeps one for its whole life), so
        // Podman may have appeared since. Probing again here keeps this check
        // from contradicting the daemon's own live probe at the request
        // boundary (TOCTOU).
        let (mut engine, mut available) = self.detected_engine().await;
        if !available && probe_podman_available() {
            // detect_engine only ever selects podman, so a successful probe
            // is exactly the engine the constructor would have recorded.
            engine = "podman".to_string();
            available = true;
        }
        if !available {
            return Ok(ValidateVerdict::DeterministicReject {
                reasons: vec!["No container engine (Podman) available. Sandbox evaluation required. Please install Podman for full security.".to_string()],
                constraints: vec![],
            });
        }

        // Validate and canonicalize cwd to prevent host path exfiltration
        let canonical_cwd =
            std::fs::canonicalize(cwd).map_err(|_| anyhow::anyhow!("Invalid cwd path"))?;
        let cwd_str = canonical_cwd.to_string_lossy().to_string();

        // Normalize shell types; non-shell types never mount, so they keep
        // the documented fail-open verdict instead of being rejected.
        if let Err(verdict) = sandbox_action_decision(action_type) {
            return Ok(verdict);
        }

        // The refusal list is a blacklist; pair it with the caller's workspace
        // allowlist so any other readable host directory cannot be mounted and
        // have its contents returned in the rejection logs.
        if let Some(reason) = workspace_cwd_refusal(&canonical_cwd) {
            return Ok(ValidateVerdict::DeterministicReject {
                reasons: vec![format!("Security violation: {reason}")],
                constraints: vec![
                    "Never mount a directory outside the authenticated caller's workspace or allowed_external_dirs."
                        .to_string(),
                ],
            });
        }

        if let Some(refused) = working_directory_refusal(&cwd_str) {
            return Ok(ValidateVerdict::DeterministicReject {
                reasons: vec![format!(
                    "Security violation: Attempted to mount forbidden host path {} ({})",
                    cwd_str, refused
                )],
                constraints: vec![
                    "Never attempt to interact with or mount sensitive host system paths."
                        .to_string(),
                ],
            });
        }

        info!("Executing sandbox evaluation using engine: {}", engine);

        evaluate_with_timeout(&engine, cwd, &cwd_str, payload, SANDBOX_TIMEOUT).await
    }
}

/// What a bounded child run produced: the completed output, or the fact that
/// the limit fired and the child was killed and reaped.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RunOutcome {
    Completed(std::process::Output),
    TimedOut,
}

/// Refuse a working directory that would expose a sensitive host tree when
/// mounted read-only at `/workspace`. Matches whole path components via
/// `Path::starts_with`, so `/etcetera` and `/homework` are ordinary project
/// paths while `/etc` and `/home/user/project` are not. Returns the offending
/// forbidden path for the rejection message.
///
/// The current user's exact home directory, its ancestors, and its sensitive
/// credential children are refused as well: mounting them would expose
/// `.ssh`, `.aws`, shell history and every sibling project. `/home` remains
/// refused as a whole because #410's acceptance criteria name
/// `/home/user/project` explicitly.
pub(crate) fn working_directory_refusal(cwd: &str) -> Option<String> {
    working_directory_refusal_with_home(cwd, dirs::home_dir().as_deref())
}

/// Pure core of [`working_directory_refusal`]: takes the home directory as a
/// parameter so tests can exercise exact-home refusal without depending on
/// the host's real home path.
pub(crate) fn working_directory_refusal_with_home(
    cwd: &str,
    home: Option<&Path>,
) -> Option<String> {
    // Canonicalize the home too where possible so a symlinked home still
    // compares equal with the already-canonicalized cwd.
    let home = home.map(|h| std::fs::canonicalize(h).unwrap_or_else(|_| h.to_path_buf()));
    working_directory_refusal_with_home_and_case(cwd, home.as_deref(), fold_case_paths())
}

/// Windows and (default) macOS filesystems are case-insensitive: two paths
/// with different casing resolve to the same directory. Folding there makes
/// the refusal checks agree with the filesystem instead of trusting spelling.
fn fold_case_paths() -> bool {
    cfg!(windows) || cfg!(target_os = "macos")
}

fn identity_components(path: &Path, fold_case: bool) -> Vec<String> {
    path.components()
        .map(|component| {
            let part = component.as_os_str().to_string_lossy().into_owned();
            if fold_case {
                part.to_lowercase()
            } else {
                part
            }
        })
        .collect()
}

fn identity_eq(a: &Path, b: &Path, fold_case: bool) -> bool {
    identity_components(a, fold_case) == identity_components(b, fold_case)
}

fn identity_starts_with(path: &Path, prefix: &Path, fold_case: bool) -> bool {
    let path = identity_components(path, fold_case);
    let prefix = identity_components(prefix, fold_case);
    path.len() >= prefix.len() && path[..prefix.len()] == prefix[..]
}

/// Refuse when the cwd is the sensitive path, lives under it, or is one of
/// its ancestors — all three shapes expose the same subtree when mounted.
fn sensitive_overlap(cwd: &Path, sensitive: &Path, fold_case: bool) -> bool {
    identity_eq(cwd, sensitive, fold_case)
        || identity_starts_with(cwd, sensitive, fold_case)
        || identity_starts_with(sensitive, cwd, fold_case)
}

fn working_directory_refusal_with_home_and_case(
    cwd: &str,
    home: Option<&Path>,
    fold_case: bool,
) -> Option<String> {
    let cwd = Path::new(cwd);
    if cwd.parent().is_none() {
        return Some(cwd.to_string_lossy().into_owned());
    }
    // Refuse the forbidden path, its descendants, AND its ancestors: mounting
    // `/var` or `/private` exposes `/var/run` and `/private/var/run` too.
    for dir in FORBIDDEN_WORKING_DIRECTORIES {
        let forbidden = Path::new(dir);
        if identity_starts_with(cwd, forbidden, fold_case)
            || identity_starts_with(forbidden, cwd, fold_case)
        {
            return Some(dir.to_string());
        }
    }
    // Fail closed: without a resolvable home the exact-home and sensitive
    // child rules below cannot be evaluated, so a caller-supplied cwd such as
    // `/Users/alice` could be mounted even though it is the user's home.
    let Some(home) = home else {
        return Some(
            "<home directory could not be determined; refusing sandbox mount>".to_string(),
        );
    };
    // The home directory itself, and any ancestor of it (for example
    // `/Users`), would expose every sibling home and credential tree.
    if identity_eq(cwd, home, fold_case) || identity_starts_with(home, cwd, fold_case) {
        return Some(home.to_string_lossy().into_owned());
    }
    // A checkout under home stays mountable, but credential and runtime state
    // under home does not. Canonicalize each child so a symlinked `.ssh` or
    // `.aws` cannot hide its real target behind a lexical comparison, then
    // refuse the path, its descendants, and its ancestors.
    for child in SENSITIVE_HOME_CHILDREN {
        let sensitive = home.join(child);
        let resolved = std::fs::canonicalize(&sensitive).unwrap_or_else(|_| sensitive.clone());
        for candidate in [&sensitive, &resolved] {
            if sensitive_overlap(cwd, candidate, fold_case) {
                return Some(candidate.to_string_lossy().into_owned());
            }
        }
    }
    None
}

/// Resolve the working directory again immediately before `podman run` and
/// require the same canonical target that passed validation. `Err` carries
/// the reason the caller must reject rather than mount.
fn revalidate_mount_cwd(original: &str, validated: &str) -> Result<String, String> {
    let resolved = std::fs::canonicalize(original)
        .map_err(|e| format!("working directory disappeared before launch: {original} ({e})"))?
        .to_string_lossy()
        .into_owned();
    if resolved != validated {
        return Err(format!(
            "working directory changed between validation and launch: {validated} -> {resolved}"
        ));
    }
    if let Some(refused) = working_directory_refusal(&resolved) {
        return Err(format!(
            "mount target refused at launch: {resolved} ({refused})"
        ));
    }
    Ok(resolved)
}

/// Evaluate a payload in the sandbox, mapping a timeout to a rejection only
/// after the child has been killed and reaped.
///
/// Launch-time revalidation narrows the check-to-mount window but cannot close
/// it in user space: the kernel resolves `podman`'s bind mount after this
/// check, so a process with write access to a parent directory may still swap
/// a component in that gap. Closing it fully needs a pinned directory handle
/// (`openat`/`O_PATH`), which the `podman run` CLI cannot express. The residual
/// OS-level TOCTOU is documented in `docs/features-api/ops/security.md`.
async fn evaluate_with_timeout(
    engine: &str,
    original_cwd: &str,
    cwd: &str,
    payload: &str,
    limit: Duration,
) -> anyhow::Result<ValidateVerdict> {
    let start = std::time::Instant::now();

    // One process-wide permit per live run, held across spawn/timeout/cleanup
    // so concurrent validations cannot collectively exhaust the host.
    let permit = match acquire_sandbox_permit(SANDBOX_QUEUE_TIMEOUT).await {
        Ok(permit) => permit,
        Err(reason) => {
            return Ok(ValidateVerdict::DeterministicReject {
                reasons: vec![format!("Security violation: {reason}")],
                constraints: vec![
                    "Never hold a sandbox payload beyond the bounded queue deadline or limit."
                        .to_string(),
                ],
            });
        }
    };

    let cwd_for_mount = match revalidate_mount_cwd(original_cwd, cwd) {
        Ok(path) => path,
        Err(reason) => {
            return Ok(ValidateVerdict::DeterministicReject {
                reasons: vec![format!("Security violation: {reason}")],
                constraints: vec![
                    "Never mount a working directory that changed or cannot be revalidated at launch."
                        .to_string(),
                ],
            });
        }
    };

    let container = format!("neurostrata-sandbox-{}", uuid::Uuid::new_v4());
    // If this future is cancelled or dropped, the named container must still
    // be removed: killing the `podman run` client does not stop the container.
    let mut cleanup_guard = ContainerCleanupGuard::new(engine, &container, permit);
    let child = sandbox_command(engine, &cwd_for_mount, &container).spawn()?;
    let (outcome, cleanup_failure) = match run_child_with_timeout_named(
        child,
        payload,
        limit,
        Some((engine, container.clone())),
    )
    .await
    {
        Ok(result) => result,
        Err(e) => {
            warn!("Sandbox execution failed to run: {}", e);
            return Ok(ValidateVerdict::SandboxReject {
                reasons: vec![format!("Sandbox execution error: {}", e)],
                logs: String::new(),
                constraints: vec![],
            });
        }
    };
    // The timeout path already attempted removal. Disarm only on success so a
    // failed removal gets a second chance from the guard's drop.
    if cleanup_failure.is_none() {
        cleanup_guard.disarm();
    }
    let duration_ms = start.elapsed().as_millis() as u64;

    if let Some(failure) = cleanup_failure {
        let logs = match &outcome {
            RunOutcome::Completed(output) => format!(
                "STDOUT:\n{}\nSTDERR:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ),
            RunOutcome::TimedOut => String::new(),
        };
        return Ok(ValidateVerdict::SandboxReject {
            reasons: vec![failure],
            logs,
            constraints: vec![],
        });
    }

    match outcome {
        RunOutcome::Completed(output) => {
            let mut stdout = String::from_utf8_lossy(&output.stdout).into_owned();
            let mut stderr = String::from_utf8_lossy(&output.stderr).into_owned();

            // Truncate logs to prevent LLM context overflow which causes death spirals and compaction errors
            if stdout.len() > 500 {
                let suffix = safe_utf8_suffix(&stdout, 500);
                stdout = format!(
                    "...[TRUNCATED {} bytes]...\n{}",
                    stdout.len() - suffix.len(),
                    suffix
                );
            }
            if stderr.len() > 500 {
                let suffix = safe_utf8_suffix(&stderr, 500);
                stderr = format!(
                    "...[TRUNCATED {} bytes]...\n{}",
                    stderr.len() - suffix.len(),
                    suffix
                );
            }

            if output.status.success() {
                Ok(ValidateVerdict::SandboxPassLowFidelity {
                    notes: vec![format!("Execution succeeded in {}ms", duration_ms)],
                })
            } else {
                // It failed the dry-run
                Ok(ValidateVerdict::SandboxReject {
                    reasons: vec![format!(
                        "Sandbox returned exit code: {:?}",
                        output.status.code()
                    )],
                    logs: format!("STDOUT:\n{}\nSTDERR:\n{}", stdout, stderr),
                    constraints: vec![], // Injected later by main
                })
            }
        }
        RunOutcome::TimedOut => {
            warn!("Sandbox execution timed out after {:?}", limit);
            Ok(ValidateVerdict::SandboxReject {
                reasons: vec!["Sandbox timed out".to_string()],
                logs: String::new(),
                constraints: vec![],
            })
        }
    }
}

/// The evaluation command: hardened `podman run` arguments with the payload
/// streamed over stdin. `kill_on_drop` is a backstop for cancellation; the
/// timeout path in `run_child_with_timeout` still kills and reaps explicitly.
fn sandbox_command(engine: &str, cwd: &str, container: &str) -> Command {
    let mut cmd = Command::new(engine);
    let mut args = sandbox_run_args(cwd);
    // A stable name lets the timeout path stop the container itself, not just
    // the `podman run` client process. `--rm` only runs after the container
    // exits.
    args.insert(1, "--name".to_string());
    args.insert(2, container.to_string());
    cmd.args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    cmd
}

/// Fixed CLI arguments for `podman run`. The shell payload is piped over
/// `stdin` to `sh -s` rather than passed via `-c <payload>` on the process
/// command line, preventing command-line injection (`rust/command-line-injection`)
/// and keeping untrusted payloads out of host `argv` / `/proc/<pid>/cmdline`.
/// The hardening set bounds what the payload can do inside: no capabilities,
/// bounded pids and memory, no network, a read-only project mount, and a
/// tmpfs scratch directory.
pub(crate) fn sandbox_run_args(cwd: &str) -> Vec<String> {
    vec![
        "run".to_string(),
        "--rm".to_string(),
        "-i".to_string(),
        "--cap-drop=ALL".to_string(),
        "--pids-limit=256".to_string(),
        "--memory=512m".to_string(),
        "--network=none".to_string(),
        "--read-only".to_string(),
        "-v".to_string(),
        format!("{}:/workspace:ro", cwd),
        format!("--tmpfs=/tmp:size={SANDBOX_TMPFS_SIZE}"),
        "-w=/workspace".to_string(),
        SANDBOX_IMAGE.to_string(),
        "sh".to_string(),
        "-s".to_string(),
    ]
}

/// Write the payload to the child's stdin, run the child under `limit`, and
/// always leave no child behind: on timeout the child is killed and then
/// reaped with `wait()` (a bare `start_kill` leaves a zombie and the pid
/// stays visible to `kill -0`).
///
/// The stdout/stderr drains start before stdin is touched, and the stdin
/// write plus `wait()` share one `limit` deadline. Either ordering mistake
/// deadlocks: a child that writes more than a pipe buffer to stdout while we
/// are still feeding stdin blocks on a full pipe, and a child that never
/// reads stdin (`sleep 60`) blocks `write_all` forever, letting the sandbox
/// outlive its timeout.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) async fn run_child_with_timeout(
    child: tokio::process::Child,
    payload: &str,
    limit: Duration,
) -> std::io::Result<RunOutcome> {
    run_child_with_timeout_named(child, payload, limit, None)
        .await
        .map(|(outcome, _)| outcome)
}

async fn run_child_with_timeout_named(
    mut child: tokio::process::Child,
    payload: &str,
    limit: Duration,
    container: Option<(&str, String)>,
) -> std::io::Result<(RunOutcome, Option<String>)> {
    let mut stdout_task = tokio::spawn(read_pipe(child.stdout.take()));
    let mut stderr_task = tokio::spawn(read_pipe(child.stderr.take()));

    let wait = timeout(limit, async {
        if let Some(mut stdin) = child.stdin.take() {
            use tokio::io::AsyncWriteExt;
            let bytes = payload.as_bytes();
            if stdin.write_all(bytes).await.is_ok() && !bytes.ends_with(b"\n") {
                let _ = stdin.write_all(b"\n").await;
            }
            let _ = stdin.shutdown().await;
        }
        child.wait().await
    })
    .await;

    match wait {
        Ok(status) => {
            let status = status?;
            // The child has exited, but a descendant that inherited stdout
            // or stderr can hold a pipe open indefinitely, so EOF is not
            // guaranteed. Bound each drain await and abandon (abort) a
            // drain that outlives the bound instead of hanging the verdict
            // past the run limit.
            const PIPE_DRAIN_TIMEOUT: Duration = Duration::from_secs(1);
            let drained = timeout(PIPE_DRAIN_TIMEOUT, async {
                tokio::join!(&mut stdout_task, &mut stderr_task)
            })
            .await;
            let ((stdout, stdout_error), (stderr, stderr_error)) = match drained {
                Ok((stdout, stderr)) => (stdout.unwrap_or_default(), stderr.unwrap_or_default()),
                Err(_) => {
                    stdout_task.abort();
                    stderr_task.abort();
                    warn!("Sandbox: output drains exceeded shared {PIPE_DRAIN_TIMEOUT:?} after child exit; abandoning captured output");
                    ((Vec::new(), None), (Vec::new(), None))
                }
            };
            let capture_errors: Vec<String> =
                [stdout_error, stderr_error].into_iter().flatten().collect();
            for error in &capture_errors {
                warn!("Sandbox: output capture failed: {error}");
            }
            let mut stderr = stderr;
            if !capture_errors.is_empty() {
                // Surface the failure in the verdict's diagnostics rather than
                // presenting a partial capture as if it were the full output.
                let note = format!(
                    "\n[sandbox output capture errors: {}]",
                    capture_errors.join("; ")
                );
                stderr.extend_from_slice(note.as_bytes());
            }
            Ok((
                RunOutcome::Completed(std::process::Output {
                    status,
                    stdout,
                    stderr,
                }),
                None,
            ))
        }
        Err(_) => {
            if let Err(e) = child.start_kill() {
                warn!("Sandbox timeout: failed to kill child: {}", e);
            }
            // Reap the killed child before returning, with BOTH waits bounded
            // so a stubborn child cannot keep the permit past the verdict.
            const POST_KILL_WAIT_TIMEOUT: Duration = Duration::from_secs(1);
            let _reaped = reap_after_kill(&mut child, POST_KILL_WAIT_TIMEOUT).await;
            // Killing the `podman run` client does not stop the container it
            // created. Remove it by the name passed to `--name`, and surface
            // any failed removal in the verdict instead of logging it away.
            let mut cleanup_failure = None;
            if let Some((engine, name)) = container {
                let mut cleanup = tokio::process::Command::new(engine);
                cleanup
                    .args(["rm", "-f", &name])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null());
                match cleanup_with_timeout(cleanup, Duration::from_secs(2)).await {
                    Ok(outcome) => {
                        if let Some(reason) = outcome.failure_reason(&name) {
                            warn!("Sandbox timeout: {reason}");
                            cleanup_failure = Some(reason);
                        } else if outcome.timed_out {
                            warn!("Sandbox timeout: container cleanup for {name} exceeded its deadline; killed and reaped the cleanup child");
                        }
                    }
                    Err(e) => {
                        let reason = format!("Sandbox container cleanup for {name} failed: {e}");
                        warn!("{reason}");
                        cleanup_failure = Some(reason);
                    }
                }
            }
            stdout_task.abort();
            stderr_task.abort();
            Ok((RunOutcome::TimedOut, cleanup_failure))
        }
    }
}

/// Kill then reap the child, but never hold the run permit indefinitely: the
/// escalation itself is bounded, and an unreapable child returns `false` so
/// the verdict can still be delivered.
async fn reap_after_kill(child: &mut tokio::process::Child, post_wait: Duration) -> bool {
    match timeout(post_wait, child.wait()).await {
        Ok(Ok(_)) => true,
        Ok(Err(e)) => {
            warn!("Sandbox timeout: failed to reap child: {e}");
            false
        }
        Err(_) => {
            warn!("Sandbox timeout: child did not exit within {post_wait:?}; escalating kill");
            if let Err(e) = child.start_kill() {
                warn!("Sandbox timeout: escalated kill failed: {e}");
            }
            match timeout(post_wait, child.wait()).await {
                Ok(Ok(_)) => true,
                Ok(Err(e)) => {
                    warn!("Sandbox timeout: failed to reap child after escalation: {e}");
                    false
                }
                Err(_) => {
                    warn!("Sandbox timeout: child still unreaped after bounded escalation; returning verdict");
                    false
                }
            }
        }
    }
}

/// Outcome of a deadline-bounded cleanup subprocess.
struct CleanupOutcome {
    #[allow(dead_code)]
    pid: Option<u32>,
    timed_out: bool,
    succeeded: bool,
}

impl CleanupOutcome {
    fn failure_reason(&self, name: &str) -> Option<String> {
        if self.timed_out {
            Some(format!(
                "Container cleanup for {name} exceeded its deadline and the cleanup child was killed"
            ))
        } else if !self.succeeded {
            Some(format!(
                "Container cleanup for {name} exited nonzero; the container may still be running"
            ))
        } else {
            None
        }
    }
}

/// Removes a named container if a sandbox run is cancelled or dropped before
/// its own cleanup path completes. The guard OWNS the run permit: the
/// synchronous, bounded removal finishes in `Drop` before the permit is
/// released, so a new run can never start beside a still-live container.
struct ContainerCleanupGuard {
    engine: String,
    name: String,
    armed: bool,
    _permit: tokio::sync::SemaphorePermit<'static>,
}

impl ContainerCleanupGuard {
    fn new(engine: &str, name: &str, permit: tokio::sync::SemaphorePermit<'static>) -> Self {
        Self {
            engine: engine.to_string(),
            name: name.to_string(),
            armed: true,
            _permit: permit,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ContainerCleanupGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let result = remove_container_sync(&self.engine, &self.name, Duration::from_secs(2));
        if let Some(reason) = sync_cleanup_failure(result, &self.name) {
            warn!("Sandbox cancellation: {reason}");
        }
    }
}

/// An unsuccessful or failed synchronous removal is a cleanup failure, not a
/// success to be logged and ignored.
fn sync_cleanup_failure(result: std::io::Result<bool>, name: &str) -> Option<String> {
    match result {
        Ok(true) => None,
        Ok(false) => Some(format!(
            "Container cleanup for {name} was unsuccessful; the container may still be running"
        )),
        Err(e) => Some(format!("Container cleanup for {name} failed: {e}")),
    }
}

/// Bounded synchronous `podman rm -f`, used from the cancellation guard.
/// Returns whether the removal itself exited successfully.
fn remove_container_sync(engine: &str, name: &str, limit: Duration) -> std::io::Result<bool> {
    let mut child = std::process::Command::new(engine)
        .args(["rm", "-f", name])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let deadline = std::time::Instant::now() + limit;
    loop {
        match child.try_wait()? {
            Some(status) => return Ok(status.success()),
            None if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            None => {
                child.kill()?;
                // Bound the reap too: an unbounded `wait()` here would keep
                // the caller's permit after the cleanup deadline fired.
                let reap_deadline = std::time::Instant::now() + Duration::from_millis(500);
                loop {
                    match child.try_wait()? {
                        Some(_) => break,
                        None if std::time::Instant::now() < reap_deadline => {
                            std::thread::sleep(Duration::from_millis(20));
                        }
                        None => {
                            warn!("Sandbox cleanup: cleanup child {name} still unreaped after bounded wait");
                            break;
                        }
                    }
                }
                return Ok(false);
            }
        }
    }
}

/// Spawn a container-removal subprocess that cannot outlive its deadline.
/// `kill_on_drop` is a backstop; on timeout the child is explicitly killed
/// and reaped so a stalled `podman rm` leaves neither a live process nor a
/// zombie after the verdict.
async fn cleanup_with_timeout(
    mut cmd: tokio::process::Command,
    limit: Duration,
) -> std::io::Result<CleanupOutcome> {
    let mut child = cmd.kill_on_drop(true).spawn()?;
    let pid = child.id();
    match timeout(limit, child.wait()).await {
        Ok(status) => {
            let succeeded = status?.success();
            Ok(CleanupOutcome {
                pid,
                timed_out: false,
                succeeded,
            })
        }
        Err(_) => {
            if let Err(e) = child.start_kill() {
                warn!("Sandbox cleanup: failed to kill stalled cleanup child {pid:?}: {e}");
            }
            const CLEANUP_REAP_TIMEOUT: Duration = Duration::from_secs(1);
            match timeout(CLEANUP_REAP_TIMEOUT, child.wait()).await {
                Ok(Ok(_)) => {}
                Ok(Err(e)) => warn!("Sandbox cleanup: failed to reap child {pid:?}: {e}"),
                Err(_) => warn!(
                    "Sandbox cleanup: child {pid:?} did not exit within {CLEANUP_REAP_TIMEOUT:?}"
                ),
            }
            Ok(CleanupOutcome {
                pid,
                timed_out: true,
                succeeded: false,
            })
        }
    }
}

async fn read_pipe<R>(pipe: Option<R>) -> (Vec<u8>, Option<String>)
where
    R: tokio::io::AsyncRead + Unpin,
{
    // Keep the tail bounded while still draining to EOF. `read_to_end` into a
    // host Vec lets a payload that writes faster than the timeout grow the
    // daemon's memory without limit; the verdict keeps only the last 500
    // bytes anyway.
    const CAPTURE_LIMIT: usize = 64 * 1024;
    // Pin the element type: `Extend<&u8>` and the blanket `Extend<T>` make
    // `with_capacity` ambiguous on Windows, where `collect` below compounds it.
    let mut buf: std::collections::VecDeque<u8> =
        std::collections::VecDeque::with_capacity(CAPTURE_LIMIT);
    let mut failure = None;
    if let Some(mut pipe) = pipe {
        use tokio::io::AsyncReadExt;
        let mut chunk = [0u8; 16 * 1024];
        loop {
            match pipe.read(&mut chunk).await {
                Ok(0) => break,
                Err(e) => {
                    // Keep the bytes already captured but tell the caller why
                    // the tail is missing instead of hiding the read error.
                    failure = Some(e.to_string());
                    break;
                }
                Ok(read) => {
                    buf.extend(chunk[..read].iter().copied());
                    if buf.len() > CAPTURE_LIMIT {
                        let excess = buf.len() - CAPTURE_LIMIT;
                        buf.drain(..excess);
                    }
                }
            }
        }
    }
    (buf.into(), failure)
}

/// UTF-8-safe tail slice: never splits a multi-byte codepoint, so indexing
/// never panics (AJ-5).
fn safe_utf8_suffix(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut idx = s.len() - max_bytes;
    while idx < s.len() && !s.is_char_boundary(idx) {
        idx += 1;
    }
    &s[idx..]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The semaphore tests share one process-global semaphore; serialize them
    /// so the default parallel runner cannot make one consume the other's
    /// permits and fail intermittently.
    static TEST_SEM_LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();

    fn semaphore_test_lock() -> &'static tokio::sync::Mutex<()> {
        TEST_SEM_LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
    }

    async fn lock_semaphore_async() -> tokio::sync::MutexGuard<'static, ()> {
        semaphore_test_lock().lock().await
    }

    fn lock_semaphore_sync() -> tokio::sync::MutexGuard<'static, ()> {
        semaphore_test_lock().blocking_lock()
    }

    fn registry_suite_has_test(registry: &str, suite: &str, name: &str) -> bool {
        let value: serde_json::Value = match serde_json::from_str(registry) {
            Ok(value) => value,
            Err(_) => return false,
        };
        value
            .get("suites")
            .and_then(|suites| suites.get(suite))
            .and_then(|entry| entry.get("tests"))
            .and_then(|tests| tests.as_array())
            .map(|tests| tests.iter().any(|test| test.as_str() == Some(name)))
            .unwrap_or(false)
    }

    #[test]
    fn sandbox_image_is_digest_pinned_and_never_latest() {
        let image = SANDBOX_IMAGE;
        assert!(
            image.starts_with("alpine@sha256:"),
            "sandbox image must be referenced by digest, not a tag: {image}"
        );
        let digest = image
            .strip_prefix("alpine@sha256:")
            .expect("alpine@sha256:<digest>");
        assert_eq!(digest.len(), 64, "a full sha256 hex digest: {image}");
        assert!(
            digest.chars().all(|c| c.is_ascii_hexdigit()),
            "digest is hex: {digest}"
        );
        assert_eq!(digest, SANDBOX_IMAGE_DIGEST);
        assert_eq!(SANDBOX_IMAGE_DIGEST.len(), 64);
        assert!(!image.contains(":latest"), "never the floating latest tag");
        assert!(
            !image.ends_with(":3.21"),
            "a version tag can be re-pushed; pin the digest"
        );
    }

    #[test]
    fn sandbox_args_pin_the_hardening_set() {
        let args = sandbox_run_args("/work/project");
        assert_eq!(
            args,
            vec![
                "run".to_string(),
                "--rm".to_string(),
                "-i".to_string(),
                "--cap-drop=ALL".to_string(),
                "--pids-limit=256".to_string(),
                "--memory=512m".to_string(),
                "--network=none".to_string(),
                "--read-only".to_string(),
                "-v".to_string(),
                "/work/project:/workspace:ro".to_string(),
                format!("--tmpfs=/tmp:size={SANDBOX_TMPFS_SIZE}"),
                "-w=/workspace".to_string(),
                SANDBOX_IMAGE.to_string(),
                "sh".to_string(),
                "-s".to_string(),
            ]
        );
        assert!(
            !args.iter().any(|a| a.as_str() == "-c"),
            "sandbox must read payload from stdin via `sh -s`, never `sh -c <payload>`"
        );
    }

    #[test]
    fn forbidden_working_directories_are_refused() {
        for cwd in [
            "/",
            "/etc",
            "/home",
            "/home/user/project",
            "/run",
            "/private/var/run",
            "/var/run/docker.sock",
            "/var/run",
            "/root",
            "/sys",
            "/dev",
            "/proc",
            "/boot",
            "/etc/passwd",
        ] {
            assert!(
                working_directory_refusal(cwd).is_some(),
                "working directory must be refused: {cwd}"
            );
        }

        for cwd in ["/etcetera", "/homework", "/work/project"] {
            assert_eq!(
                working_directory_refusal_with_home(cwd, Some(Path::new("/Users/alice"))),
                None,
                "component prefix, not a string prefix, must decide: {cwd}"
            );
        }
    }

    #[test]
    fn exact_home_directory_is_refused_but_descendants_are_allowed() {
        let home = Path::new("/Users/alice");
        assert_eq!(
            working_directory_refusal_with_home("/Users/alice", Some(home)).as_deref(),
            Some("/Users/alice"),
            "mounting the exact home directory would expose ~/.ssh and every sibling project"
        );
        assert_eq!(
            working_directory_refusal_with_home("/Users/alice/", Some(home)).as_deref(),
            Some("/Users/alice"),
            "a trailing separator still names the home directory"
        );
        assert_eq!(
            working_directory_refusal_with_home("/Users/alice/project/sub", Some(home)),
            None,
            "a project checkout under home stays a valid working directory"
        );
        // No home known (e.g. HOME/USERPROFILE unset): the exact-home and
        // sensitive-child rules cannot be evaluated, so fail closed rather
        // than mounting a directory that may be the caller's home.
        assert!(
            working_directory_refusal_with_home("/Users/alice", None).is_some(),
            "an unresolved home must fail closed"
        );
        assert_eq!(
            working_directory_refusal_with_home("/etc", Some(home)).as_deref(),
            Some("/etc")
        );
        // An ancestor of home exposes every sibling home, so it is refused.
        assert_eq!(
            working_directory_refusal_with_home("/Users", Some(home)).as_deref(),
            Some("/Users/alice")
        );
        // Credential trees under home are refused even when they are not home
        // itself; ordinary project directories stay allowed.
        for cwd in [
            "/Users/alice/.ssh",
            "/Users/alice/.ssh/id_ed25519",
            "/Users/alice/.aws/credentials",
            "/Users/alice/Library/Keychains/login.keychain-db",
        ] {
            assert!(
                working_directory_refusal_with_home(cwd, Some(home)).is_some(),
                "sensitive home path must be refused: {cwd}"
            );
        }
        assert_eq!(
            working_directory_refusal_with_home("/Users/bob/project", Some(home)),
            None
        );
        assert_eq!(
            working_directory_refusal_with_home("/Users/alice2", Some(home)),
            None,
            "component equality, not a string prefix"
        );
    }

    #[test]
    fn sensitive_home_path_ancestors_are_refused() {
        let home = Path::new("/Users/alice");
        // The parent of a sensitive child mounts the credential tree too.
        for cwd in [
            "/Users/alice/Library",
            "/Users/alice/.local",
            "/Users/alice/.local/share",
        ] {
            assert!(
                working_directory_refusal_with_home(cwd, Some(home)).is_some(),
                "an ancestor of a sensitive home child must be refused: {cwd}"
            );
        }
    }

    #[test]
    fn ancestors_of_forbidden_system_paths_are_refused() {
        let home = Path::new("/Users/alice");
        for cwd in ["/var", "/private", "/private/var"] {
            assert!(
                working_directory_refusal_with_home(cwd, Some(home)).is_some(),
                "an ancestor of a forbidden system path must be refused: {cwd}"
            );
        }
        assert!(working_directory_refusal("/var").is_some());
        // Siblings with a shared string prefix remain ordinary projects.
        assert_eq!(
            working_directory_refusal_with_home("/var/runner", Some(home)),
            None
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_sensitive_home_children_are_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path().join("home");
        let outside = dir.path().join("outside-secrets");
        std::fs::create_dir_all(&home).expect("create home");
        std::fs::create_dir_all(&outside).expect("create outside");
        std::os::unix::fs::symlink(&outside, home.join(".ssh")).expect("symlink .ssh");
        let resolved = std::fs::canonicalize(&outside).expect("canonicalize target");

        assert!(
            working_directory_refusal_with_home(&resolved.to_string_lossy(), Some(&home)).is_some(),
            "a canonicalized cwd reached through a sensitive symlink must be refused"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn both_output_pipes_share_one_drain_deadline() {
        let child = Command::new("sh")
            .arg("-s")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn sh -s");
        let started = std::time::Instant::now();
        let outcome = run_child_with_timeout(
            child,
            "(sleep 30) >&1 &\n(sleep 30) >&2 &\nprintf 'done'\nexit 0",
            Duration::from_secs(5),
        )
        .await
        .expect("run child");
        let RunOutcome::Completed(output) = outcome else {
            panic!("the parent shell completed; the run must not time out");
        };
        assert!(output.status.success());
        assert!(
            started.elapsed() < Duration::from_millis(1800),
            "two drains share one deadline; sequential 1s waits would take ~2s"
        );
    }

    #[test]
    fn unknown_home_fails_closed() {
        for cwd in ["/Users/alice", "/work/project", "/Users/alice/project"] {
            assert!(
                working_directory_refusal_with_home(cwd, None).is_some(),
                "an unresolved home must refuse the mount: {cwd}"
            );
        }
    }

    #[test]
    fn a_project_working_directory_is_allowed() {
        for cwd in [
            "/work/project",
            "/Users/neo/src/project",
            "/homes/project",
            "/var/runner",
            "/devices/project",
        ] {
            assert_eq!(
                working_directory_refusal_with_home(cwd, Some(Path::new("/Users/alice"))),
                None,
                "an ordinary project directory stays allowed: {cwd}"
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_timed_out_run_kills_and_reaps_its_child() {
        let child = Command::new("sh")
            .arg("-c")
            .arg("sleep 60")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn sleep");
        let pid = child.id().expect("child pid");

        let started = std::time::Instant::now();
        let outcome = run_child_with_timeout(child, "", Duration::from_millis(200))
            .await
            .expect("run child");
        assert_eq!(outcome, RunOutcome::TimedOut);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "the run returns promptly after the limit"
        );

        let status = std::process::Command::new("kill")
            .arg("-0")
            .arg(pid.to_string())
            .stderr(Stdio::null())
            .status()
            .expect("run kill -0");
        assert!(
            !status.success(),
            "timed-out child {pid} must be killed and reaped, not left running or zombie"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_large_payload_to_a_child_that_never_reads_stdin_times_out_and_reaps() {
        let child = Command::new("sh")
            .arg("-c")
            .arg("sleep 60")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn sleep");
        let pid = child.id().expect("child pid");

        // >128 KiB exceeds the 64 KiB pipe buffer, so `write_all` blocks until
        // the child reads. `sleep 60` never reads stdin: the write itself must
        // be bounded by the same limit as `wait`, or the sandbox outlives it.
        let payload = "x".repeat(128 * 1024);
        let started = std::time::Instant::now();
        let outcome = tokio::time::timeout(
            Duration::from_secs(5),
            run_child_with_timeout(child, &payload, Duration::from_secs(1)),
        )
        .await
        .expect("a blocked stdin write must be bounded by the run limit, not hang")
        .expect("run child");
        assert_eq!(outcome, RunOutcome::TimedOut);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "the blocked stdin write returns promptly after the limit"
        );

        let status = std::process::Command::new("kill")
            .arg("-0")
            .arg(pid.to_string())
            .stderr(Stdio::null())
            .status()
            .expect("run kill -0");
        assert!(
            !status.success(),
            "timed-out child {pid} must be killed and reaped, not left running or zombie"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_descendant_holding_a_pipe_does_not_hang_the_verdict() {
        let child = Command::new("sh")
            .arg("-s")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn sh -s");

        // The background subshell inherits stdout and keeps the pipe open
        // for 30s after `sh` itself exits; the drain awaits must be bounded
        // so the completed verdict returns promptly with the child status.
        let started = std::time::Instant::now();
        let outcome = run_child_with_timeout(
            child,
            "(sleep 30) &\nprintf 'parent-done'\nexit 0",
            Duration::from_secs(5),
        )
        .await
        .expect("run child");
        let RunOutcome::Completed(output) = outcome else {
            panic!("the parent shell completed; the run must not time out");
        };
        assert!(output.status.success());
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "a descendant holding a pipe must not hang the verdict past the drain bound"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_child_with_timeout_streams_to_sh_stdin_and_preserves_status() {
        let ok_child = Command::new("sh")
            .arg("-s")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn sh -s");
        let ok_out =
            run_child_with_timeout(ok_child, "printf 'sandbox-ok'", Duration::from_secs(5))
                .await
                .expect("wait sh -s");
        let RunOutcome::Completed(ok_out) = ok_out else {
            panic!("a fast sh -s must complete, not time out");
        };
        assert!(ok_out.status.success());
        assert_eq!(String::from_utf8_lossy(&ok_out.stdout), "sandbox-ok");

        let fail_child = Command::new("sh")
            .arg("-s")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn failing sh -s");
        let fail_out =
            run_child_with_timeout(fail_child, "echo 'bad' >&2\nexit 7", Duration::from_secs(5))
                .await
                .expect("wait failing sh -s");
        let RunOutcome::Completed(fail_out) = fail_out else {
            panic!("a fast failing sh -s must complete, not time out");
        };
        assert_eq!(fail_out.status.code(), Some(7));
        assert_eq!(String::from_utf8_lossy(&fail_out.stderr).trim(), "bad");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stalled_cleanup_child_is_killed_and_reaped() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "exec sleep 60"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let started = std::time::Instant::now();
        let outcome = cleanup_with_timeout(cmd, Duration::from_millis(200))
            .await
            .expect("run bounded cleanup");
        assert!(outcome.timed_out, "a stalled cleanup must hit its deadline");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "cleanup returns promptly after the deadline"
        );
        let pid = outcome.pid.expect("cleanup child pid");
        let status = std::process::Command::new("kill")
            .arg("-0")
            .arg(pid.to_string())
            .stderr(Stdio::null())
            .status()
            .expect("run kill -0");
        assert!(
            !status.success(),
            "stalled cleanup child {pid} must be killed and reaped"
        );
    }

    #[cfg(unix)]
    #[test]
    fn mount_cwd_revalidation_detects_a_swapped_symlink() {
        let dir = tempfile::tempdir().expect("tempdir");
        let real_a = dir.path().join("a");
        let real_b = dir.path().join("b");
        std::fs::create_dir_all(&real_a).expect("create a");
        std::fs::create_dir_all(&real_b).expect("create b");
        let link = dir.path().join("workspace");
        std::os::unix::fs::symlink(&real_a, &link).expect("symlink to a");

        let original = link.to_string_lossy().into_owned();
        let validated = std::fs::canonicalize(&link)
            .expect("canonicalize link")
            .to_string_lossy()
            .into_owned();
        assert_eq!(
            revalidate_mount_cwd(&original, &validated).as_deref(),
            Ok(validated.as_str())
        );

        // Swap the symlink after validation: the mount target is no longer
        // the directory the refusal check inspected.
        std::fs::remove_file(&link).expect("remove link");
        std::os::unix::fs::symlink(&real_b, &link).expect("symlink to b");
        let err = revalidate_mount_cwd(&original, &validated).unwrap_err();
        assert!(
            err.contains("changed between validation and launch"),
            "symlink swap must be rejected: {err}"
        );
    }

    #[test]
    fn action_types_are_normalized_or_rejected_under_sandbox() {
        assert_eq!(sandbox_action_type("  BASH "), Ok("bash"));
        assert_eq!(sandbox_action_type("Script"), Ok("script"));
        assert!(
            sandbox_action_type("write").is_err(),
            "an unsupported action must fail closed when sandboxing was requested"
        );
        assert!(sandbox_action_type("bash -c").is_err());
    }

    #[test]
    fn case_variant_home_paths_are_refused_on_case_insensitive_filesystems() {
        let home = Path::new("/Users/alice");
        assert_eq!(
            working_directory_refusal_with_home_and_case("/USERS/ALICE", Some(home), true)
                .as_deref(),
            Some("/Users/alice")
        );
        assert!(working_directory_refusal_with_home_and_case(
            "/USERS/ALICE/.SSH/id_ed25519",
            Some(home),
            true,
        )
        .is_some());
        assert_eq!(
            working_directory_refusal_with_home_and_case("/Users/alice/project", Some(home), true),
            None
        );
        // A case-sensitive filesystem keeps the spellings distinct.
        assert_eq!(
            working_directory_refusal_with_home_and_case("/USERS/ALICE", Some(home), false),
            None
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_full_sandbox_queue_rejects_within_its_deadline() {
        let _guard = lock_semaphore_async().await;
        let sem = sandbox_concurrency();
        let held: Vec<_> = (0..effective_sandbox_runs())
            .map(|_| sem.try_acquire().expect("permit"))
            .collect();
        let started = std::time::Instant::now();
        let err = acquire_sandbox_permit(Duration::from_millis(50))
            .await
            .expect_err("a full queue must be rejected, not waited on forever");
        assert!(err.contains("queue"), "unexpected rejection: {err}");
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "queueing must be bounded by the deadline"
        );
        drop(held);
        assert!(acquire_sandbox_permit(Duration::from_millis(50))
            .await
            .is_ok());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cleanup_exit_status_is_a_failure() {
        let mut failing = Command::new("sh");
        failing
            .args(["-c", "exit 3"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let outcome = cleanup_with_timeout(failing, Duration::from_secs(2))
            .await
            .expect("run cleanup");
        assert!(!outcome.timed_out);
        assert!(
            outcome.failure_reason("neurostrata-sandbox-x").is_some(),
            "a nonzero removal must be reported, not treated as success"
        );

        let mut ok = Command::new("sh");
        ok.args(["-c", "exit 0"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let outcome = cleanup_with_timeout(ok, Duration::from_secs(2))
            .await
            .expect("run cleanup");
        assert_eq!(outcome.failure_reason("neurostrata-sandbox-x"), None);
    }

    #[cfg(unix)]
    fn write_logging_engine(engine: &std::path::Path, log: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt;
        let body = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexit 0\n",
            log.display()
        );
        std::fs::write(engine, body).expect("write engine script");
        std::fs::set_permissions(engine, std::fs::Permissions::from_mode(0o755))
            .expect("chmod engine script");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancellation_guard_removes_the_named_container() {
        let dir = tempfile::tempdir().expect("tempdir");
        let engine = dir.path().join("engine.sh");
        let log = dir.path().join("calls.log");
        write_logging_engine(&engine, &log);

        let permit = acquire_sandbox_permit(Duration::from_millis(100))
            .await
            .expect("permit");
        let guard = ContainerCleanupGuard::new(
            &engine.to_string_lossy(),
            "neurostrata-sandbox-cancelled",
            permit,
        );
        drop(guard);

        let mut calls = String::new();
        for _ in 0..50 {
            if let Ok(text) = std::fs::read_to_string(&log) {
                if text.contains("rm -f neurostrata-sandbox-cancelled") {
                    calls = text;
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            calls.contains("rm -f neurostrata-sandbox-cancelled"),
            "dropping the run must remove the named container: {calls}"
        );

        let permit = acquire_sandbox_permit(Duration::from_millis(100))
            .await
            .expect("permit");
        let mut guard =
            ContainerCleanupGuard::new(&engine.to_string_lossy(), "not-launched", permit);
        guard.disarm();
        drop(guard);
        tokio::time::sleep(Duration::from_millis(100)).await;
        let calls = std::fs::read_to_string(&log).unwrap_or_default();
        assert!(
            !calls.contains("not-launched"),
            "a disarmed guard must not remove a container"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn killed_child_is_reaped_when_descendants_hold_pipes() {
        let child = Command::new("sh")
            .arg("-s")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn sh -s");
        let pid = child.id().expect("child pid");
        let outcome = run_child_with_timeout(
            child,
            "(sleep 30) &\nprintf 'started'\nsleep 60\n",
            Duration::from_millis(200),
        )
        .await
        .expect("run child");
        assert_eq!(outcome, RunOutcome::TimedOut);
        let status = std::process::Command::new("kill")
            .arg("-0")
            .arg(pid.to_string())
            .stderr(Stdio::null())
            .status()
            .expect("run kill -0");
        assert!(
            !status.success(),
            "the killed child {pid} must be reaped before the verdict returns"
        );
    }

    #[test]
    fn hardening_registry_lists_every_sandbox_regression() {
        const REGISTRY: &str = include_str!("../../eval/registry.json");
        for name in [
            "guard::sandbox::tests::sandbox_image_is_digest_pinned_and_never_latest",
            "guard::sandbox::tests::sandbox_args_pin_the_hardening_set",
            "guard::sandbox::tests::forbidden_working_directories_are_refused",
            "guard::sandbox::tests::exact_home_directory_is_refused_but_descendants_are_allowed",
            "guard::sandbox::tests::a_project_working_directory_is_allowed",
            "guard::sandbox::tests::a_timed_out_run_kills_and_reaps_its_child",
            "guard::sandbox::tests::a_large_payload_to_a_child_that_never_reads_stdin_times_out_and_reaps",
            "guard::sandbox::tests::unknown_home_fails_closed",
            "guard::sandbox::tests::a_descendant_holding_a_pipe_does_not_hang_the_verdict",
            "guard::sandbox::tests::stalled_cleanup_child_is_killed_and_reaped",
            "guard::sandbox::tests::mount_cwd_revalidation_detects_a_swapped_symlink",
            "guard::sandbox::tests::sandbox_concurrency_is_bounded",
            "guard::sandbox::tests::post_kill_reap_is_bounded",
            "guard::sandbox::tests::sync_removal_failure_is_a_cleanup_failure",
            "guard::sandbox::tests::permit_is_held_until_container_cleanup_finishes",
            "guard::sandbox::tests::bounded_sync_removal_kills_a_stalled_cleanup_child",
            "guard::sandbox::tests::workspace_scope_refuses_paths_outside_the_project_root",
            "guard::sandbox::tests::effective_concurrency_bounds_aggregate_memory",
            "guard::sandbox::tests::unsupported_action_types_are_approved_fail_open",
            "guard::sandbox::tests::pipe_read_errors_are_reported_with_partial_output",
            "guard::sandbox::tests::registry_suite_membership_is_checked_not_global_contains",
            "guard::sandbox::tests::available_permits_are_not_counted_as_queue_waiters",
            "guard::sandbox::tests::sandbox_args_make_root_read_only_and_bound_scratch",
        ] {
            assert!(
                registry_suite_has_test(REGISTRY, "guard-sandbox-hardening", name),
                "the hardening suite itself must pin its regression test: {name}"
            );
        }
    }

    #[test]
    fn ordinary_project_paths_do_not_depend_on_host_home() {
        let home = Path::new("/Users/alice");
        for cwd in [
            "/work/project",
            "/Users/neo/src/project",
            "/homes/project",
            "/var/runner",
            "/devices/project",
        ] {
            assert_eq!(
                working_directory_refusal_with_home(cwd, Some(home)),
                None,
                "an ordinary project directory stays allowed: {cwd}"
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn post_kill_reap_is_bounded() {
        let mut child = Command::new("sh")
            .arg("-c")
            .arg("sleep 60")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn sleep");
        child.start_kill().expect("kill child");
        let started = std::time::Instant::now();
        let reaped = reap_after_kill(&mut child, Duration::from_millis(200)).await;
        assert!(reaped, "a killed child must be reaped");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "post-kill reaping must itself be bounded"
        );
    }

    #[test]
    fn sync_removal_failure_is_a_cleanup_failure() {
        assert!(sync_cleanup_failure(Ok(true), "c").is_none());
        assert!(sync_cleanup_failure(Ok(false), "c")
            .expect("an unsuccessful removal is a failure")
            .contains("unsuccessful"));
        let err = std::io::Error::other("boom");
        assert!(sync_cleanup_failure(Err(err), "c")
            .expect("a spawn failure is a cleanup failure")
            .contains("boom"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn permit_is_held_until_container_cleanup_finishes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let engine = dir.path().join("slow-engine.sh");
        let log = dir.path().join("calls.log");
        std::fs::write(
            &engine,
            format!(
                "#!/bin/sh\nsleep 0.3\nprintf '%s\\n' \"$*\" >> '{}'\nexit 0\n",
                log.display()
            ),
        )
        .expect("write engine");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&engine, std::fs::Permissions::from_mode(0o755))
            .expect("chmod engine");

        let permit = acquire_sandbox_permit(Duration::from_millis(100))
            .await
            .expect("permit");
        let guard = ContainerCleanupGuard::new(&engine.to_string_lossy(), "hold-permit", permit);
        drop(guard);
        let calls = std::fs::read_to_string(&log).unwrap_or_default();
        assert!(
            calls.contains("rm -f hold-permit"),
            "guard drop must finish removal before the permit is released: {calls}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn bounded_sync_removal_kills_a_stalled_cleanup_child() {
        let dir = tempfile::tempdir().expect("tempdir");
        let engine = dir.path().join("slow-rm.sh");
        std::fs::write(&engine, "#!/bin/sh\nexec sleep 60\n").expect("write engine");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&engine, std::fs::Permissions::from_mode(0o755))
            .expect("chmod engine");
        let started = std::time::Instant::now();
        let result = remove_container_sync(
            &engine.to_string_lossy(),
            "stalled",
            Duration::from_millis(200),
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "sync removal is bounded"
        );
        assert!(!result.expect("sync removal result"));
    }

    #[test]
    fn workspace_scope_refuses_paths_outside_the_project_root() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("root");
        let outside = dir.path().join("outside");
        let allowed = dir.path().join("allowed");
        for p in [&root, &outside, &allowed] {
            std::fs::create_dir_all(p).expect("create dir");
        }
        assert_eq!(workspace_cwd_refusal_with(&root, &root, &[]), None);
        assert!(
            workspace_cwd_refusal_with(&outside, &root, &[]).is_some(),
            "a readable directory outside the workspace must be refused before mounting"
        );
        assert_eq!(
            workspace_cwd_refusal_with(&allowed, &root, std::slice::from_ref(&allowed)),
            None,
            "an explicit allowlisted directory stays mountable"
        );
    }

    #[test]
    fn effective_concurrency_bounds_aggregate_memory() {
        const PER_RUN: u64 = 512 * 1024 * 1024;
        assert_eq!(effective_sandbox_runs_for(8 * PER_RUN, 8 * PER_RUN, 8), 6);
        assert_eq!(effective_sandbox_runs_for(8 * PER_RUN, 4 * PER_RUN, 8), 4);
        assert_eq!(effective_sandbox_runs_for(PER_RUN, 8 * PER_RUN, 8), 1);
        assert_eq!(
            effective_sandbox_runs_for(0, 0, 8),
            1,
            "no usable capacity still allows at most one run, never zero or eight"
        );
    }

    #[test]
    fn unsupported_action_types_are_approved_fail_open() {
        assert!(matches!(sandbox_action_decision("  BASH "), Ok("bash")));
        assert!(matches!(sandbox_action_decision("Script"), Ok("script")));
        let decision = sandbox_action_decision("write");
        assert!(
            matches!(decision, Err(ValidateVerdict::ApprovedFailOpen { .. })),
            "a valid non-shell action must keep the documented fail-open verdict: {decision:?}"
        );
    }

    #[tokio::test]
    async fn pipe_read_errors_are_reported_with_partial_output() {
        struct FailingReader {
            sent: bool,
        }
        impl tokio::io::AsyncRead for FailingReader {
            fn poll_read(
                mut self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
                buf: &mut tokio::io::ReadBuf<'_>,
            ) -> std::task::Poll<std::io::Result<()>> {
                if !self.sent {
                    self.sent = true;
                    buf.put_slice(b"partial");
                    std::task::Poll::Ready(Ok(()))
                } else {
                    std::task::Poll::Ready(Err(std::io::Error::other("pipe broke")))
                }
            }
        }
        let (data, err) = read_pipe(Some(FailingReader { sent: false })).await;
        assert_eq!(data, b"partial", "bytes read before the error are kept");
        assert!(err
            .expect("a pipe read error must be returned")
            .contains("pipe broke"));
    }

    #[test]
    fn registry_suite_membership_is_checked_not_global_contains() {
        const FIXTURE: &str =
            r#"{"suites":{"guard-sandbox-hardening":{"tests":["a"]},"other":{"tests":["b"]}}}"#;
        assert!(registry_suite_has_test(
            FIXTURE,
            "guard-sandbox-hardening",
            "a"
        ));
        assert!(
            !registry_suite_has_test(FIXTURE, "guard-sandbox-hardening", "b"),
            "a name in a different suite must not satisfy the hardening suite check"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn available_permits_are_not_counted_as_queue_waiters() {
        let _guard = lock_semaphore_async().await;
        let before = SANDBOX_WAITERS.load(std::sync::atomic::Ordering::Relaxed);
        SANDBOX_WAITERS.store(SANDBOX_MAX_QUEUED, std::sync::atomic::Ordering::Relaxed);
        let result = acquire_sandbox_permit(Duration::from_millis(100)).await;
        SANDBOX_WAITERS.store(before, std::sync::atomic::Ordering::Relaxed);
        assert!(
            result.is_ok(),
            "a free permit must not be rejected because overlapping callers inflated the queue count"
        );
    }

    #[test]
    fn sandbox_args_make_root_read_only_and_bound_scratch() {
        let args = sandbox_run_args("/work/project");
        assert!(
            args.iter().any(|a| a == "--read-only"),
            "the container root filesystem must be read-only: {args:?}"
        );
        assert!(
            args.iter().any(|a| a.starts_with("--tmpfs=/tmp:size=")),
            "writable scratch must have a bounded size: {args:?}"
        );
    }

    #[test]
    fn sandbox_concurrency_is_bounded() {
        let _guard = lock_semaphore_sync();
        assert_eq!(SANDBOX_MAX_CONCURRENT_RUNS, 8);
        let sem = sandbox_concurrency();
        let available = sem.available_permits();
        assert!(
            available <= SANDBOX_MAX_CONCURRENT_RUNS,
            "never more than the configured ceiling"
        );
        let held: Vec<_> = (0..available)
            .filter_map(|_| sem.try_acquire().ok())
            .collect();
        assert!(
            sem.try_acquire().is_err(),
            "every permit is held; an extra run must wait"
        );
        drop(held);
        assert!(
            sem.try_acquire().is_ok(),
            "permits are released after a run"
        );
    }

    #[test]
    fn safe_suffix_no_panic_inside_multibyte_char() {
        // `s.len() - 500` lands inside a multi-byte '🦀' codepoint: the naive
        // byte slice would panic, the helper must split forward instead.
        let crab = "🦀".repeat(300);
        assert_eq!(crab.len(), 1200);
        let suffix = safe_utf8_suffix(&crab, 500);
        assert!(suffix.len() <= 500);
        assert!(suffix.is_char_boundary(0));
        assert!(std::str::from_utf8(suffix.as_bytes()).is_ok());

        // Two-byte 'é' straddling the boundary.
        let accents = "é".repeat(400);
        assert_eq!(accents.len(), 800);
        let suffix = safe_utf8_suffix(&accents, 500);
        assert!(suffix.len() <= 500);
        assert!(suffix.is_char_boundary(0));
    }

    #[test]
    fn safe_suffix_short_returns_whole_string() {
        let s = "hello";
        assert_eq!(safe_utf8_suffix(s, 500), s);
        let s = "🦀🦀";
        assert_eq!(safe_utf8_suffix(s, 500), s);
    }

    #[test]
    fn safe_suffix_exact_boundary_unchanged() {
        let s = "x".repeat(500);
        assert_eq!(safe_utf8_suffix(&s, 500), s);
        assert_eq!(safe_utf8_suffix("abcdefghij", 5), "fghij");
    }
}
