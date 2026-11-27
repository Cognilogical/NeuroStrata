use crate::guard::models::ValidateVerdict;
use std::path::Path;
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

static SANDBOX_CONCURRENCY: std::sync::OnceLock<tokio::sync::Semaphore> =
    std::sync::OnceLock::new();
static SANDBOX_WAITERS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn sandbox_concurrency() -> &'static tokio::sync::Semaphore {
    SANDBOX_CONCURRENCY.get_or_init(|| tokio::sync::Semaphore::new(SANDBOX_MAX_CONCURRENT_RUNS))
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

        // Normalize first, then fail closed: a case variant such as `BASH`
        // must still be sandboxed, and an unsupported type must never receive
        // approval without the container and mount checks.
        let action_type = match sandbox_action_type(action_type) {
            Ok(normalized) => normalized,
            Err(reason) => {
                return Ok(ValidateVerdict::DeterministicReject {
                    reasons: vec![reason],
                    constraints: vec![],
                });
            }
        };

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
    let home = dirs::home_dir();
    // `evaluate` canonicalizes the cwd before calling this; canonicalize the
    // home too where possible so a symlinked home still compares equal.
    let home = home.as_deref().map(|h| {
        std::fs::canonicalize(h)
            .ok()
            .unwrap_or_else(|| h.to_path_buf())
    });
    working_directory_refusal_with_home_and_case(cwd, home.as_deref(), fold_case_paths())
}

/// Pure core of [`working_directory_refusal`]: takes the home directory as a
/// parameter so tests can exercise exact-home refusal without depending on
/// the host's real home path.
pub(crate) fn working_directory_refusal_with_home(
    cwd: &str,
    home: Option<&Path>,
) -> Option<String> {
    working_directory_refusal_with_home_and_case(cwd, home, fold_case_paths())
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
    let _permit = match acquire_sandbox_permit(SANDBOX_QUEUE_TIMEOUT).await {
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
    let mut cleanup_guard = ContainerCleanupGuard::new(engine, &container);
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
        "-v".to_string(),
        format!("{}:/workspace:ro", cwd),
        "--tmpfs=/tmp".to_string(),
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
            let (stdout, stderr) = match drained {
                Ok((stdout, stderr)) => (stdout.unwrap_or_default(), stderr.unwrap_or_default()),
                Err(_) => {
                    stdout_task.abort();
                    stderr_task.abort();
                    warn!("Sandbox: output drains exceeded shared {PIPE_DRAIN_TIMEOUT:?} after child exit; abandoning captured output");
                    (Vec::new(), Vec::new())
                }
            };
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
            // Reap the killed child before returning: a bounded first wait
            // keeps the verdict prompt, but if the child somehow survives the
            // first SIGKILL we re-kill and wait until it is actually reaped
            // rather than leaving a zombie behind the released permit.
            const POST_KILL_WAIT_TIMEOUT: Duration = Duration::from_secs(1);
            match timeout(POST_KILL_WAIT_TIMEOUT, child.wait()).await {
                Ok(Ok(_)) => {}
                Ok(Err(e)) => {
                    warn!("Sandbox timeout: failed to reap child: {}", e);
                }
                Err(_) => {
                    warn!(
                        "Sandbox timeout: child did not exit within {POST_KILL_WAIT_TIMEOUT:?}; escalating kill and waiting for reap"
                    );
                    if let Err(e) = child.start_kill() {
                        warn!("Sandbox timeout: escalated kill failed: {e}");
                    }
                    if let Err(e) = child.wait().await {
                        warn!("Sandbox timeout: failed to reap child after escalation: {e}");
                    }
                }
            }
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

/// Outcome of a deadline-bounded cleanup subprocess.
struct CleanupOutcome {
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
/// its own cleanup path completes. A `std::thread` keeps the removal bounded
/// even when the async runtime is being torn down.
struct ContainerCleanupGuard {
    engine: String,
    name: String,
    armed: bool,
}

impl ContainerCleanupGuard {
    fn new(engine: &str, name: &str) -> Self {
        Self {
            engine: engine.to_string(),
            name: name.to_string(),
            armed: true,
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
        let engine = self.engine.clone();
        let name = self.name.clone();
        std::thread::spawn(move || {
            if let Err(e) = remove_container_sync(&engine, &name, Duration::from_secs(2)) {
                warn!("Sandbox cancellation: failed to remove container {name}: {e}");
            }
        });
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
                child.wait()?;
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

async fn read_pipe<R>(pipe: Option<R>) -> Vec<u8>
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
    if let Some(mut pipe) = pipe {
        use tokio::io::AsyncReadExt;
        let mut chunk = [0u8; 16 * 1024];
        loop {
            match pipe.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
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
    buf.into()
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
                "-v".to_string(),
                "/work/project:/workspace:ro".to_string(),
                "--tmpfs=/tmp".to_string(),
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
                working_directory_refusal(cwd),
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
        let sem = sandbox_concurrency();
        let held: Vec<_> = (0..SANDBOX_MAX_CONCURRENT_RUNS)
            .map(|_| sem.try_acquire().expect("permit"))
            .collect();
        let started = std::time::Instant::now();
        let err = acquire_sandbox_permit(Duration::from_millis(50))
            .await
            .err()
            .expect("a full queue must be rejected, not waited on forever");
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

        let guard =
            ContainerCleanupGuard::new(&engine.to_string_lossy(), "neurostrata-sandbox-cancelled");
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

        let mut guard = ContainerCleanupGuard::new(&engine.to_string_lossy(), "not-launched");
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
            "guard::sandbox::tests::unknown_home_fails_closed",
            "guard::sandbox::tests::a_descendant_holding_a_pipe_does_not_hang_the_verdict",
            "guard::sandbox::tests::stalled_cleanup_child_is_killed_and_reaped",
            "guard::sandbox::tests::mount_cwd_revalidation_detects_a_swapped_symlink",
            "guard::sandbox::tests::sandbox_concurrency_is_bounded",
        ] {
            assert!(
                REGISTRY.contains(&format!("\"{name}\"")),
                "the hardening suite must pin its regression test: {name}"
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

    #[test]
    fn sandbox_concurrency_is_bounded() {
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
