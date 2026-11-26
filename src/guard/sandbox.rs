use crate::guard::models::ValidateVerdict;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::timeout;
use tracing::{info, warn};

/// The SHA-256 digest of the multi-arch `alpine:3.21` OCI index, resolved
/// from the Docker Hub registry (`Docker-Content-Digest` on the manifest
/// `HEAD`) and cross-checked against `docker pull alpine:3.21`'s
/// `RepoDigests` on 2026-11-25. `SANDBOX_IMAGE` composes the digest into the
/// `alpine@sha256:<digest>` reference below; the two must stay in sync (the
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

/// Host paths a guarded action's working directory may never sit under: the
/// sandbox mounts the directory read-only at `/workspace`, so anything here
/// would be exposed to the evaluated payload. `/` (the filesystem root) is
/// handled separately because every absolute path starts with it.
const FORBIDDEN_WORKING_DIRECTORIES: &[&str] = &[
    "/etc", "/home", "/var/run", "/root", "/sys", "/dev", "/proc", "/boot",
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

        // We only sandbox bash or known script commands for now. Any other
        // action type needs no container: it already passed the deterministic
        // and semantic rule checks, so the guard approves with a warning
        // rather than rejecting action types the sandbox was never meant to
        // run.
        if action_type != "bash" && action_type != "script" {
            return Ok(ValidateVerdict::ApprovedFailOpen {
                warning: format!(
                    "Sandbox execution not applicable for action_type '{}'; approved after deterministic and semantic rule checks",
                    action_type
                ),
            });
        }

        info!("Executing sandbox evaluation using engine: {}", engine);

        evaluate_with_timeout(&engine, &cwd_str, payload, SANDBOX_TIMEOUT).await
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
pub(crate) fn working_directory_refusal(cwd: &str) -> Option<&'static str> {
    let cwd = Path::new(cwd);
    if cwd == Path::new("/") {
        return Some("/");
    }
    FORBIDDEN_WORKING_DIRECTORIES
        .iter()
        .copied()
        .find(|dir| cwd.starts_with(dir))
}

/// Evaluate a payload in the sandbox, mapping a timeout to a rejection only
/// after the child has been killed and reaped.
async fn evaluate_with_timeout(
    engine: &str,
    cwd: &str,
    payload: &str,
    limit: Duration,
) -> anyhow::Result<ValidateVerdict> {
    let start = std::time::Instant::now();
    let child = sandbox_command(engine, cwd).spawn()?;
    let outcome = match run_child_with_timeout(child, payload, limit).await {
        Ok(outcome) => outcome,
        Err(e) => {
            warn!("Sandbox execution failed to run: {}", e);
            return Ok(ValidateVerdict::SandboxReject {
                reasons: vec![format!("Sandbox execution error: {}", e)],
                logs: String::new(),
                constraints: vec![],
            });
        }
    };
    let duration_ms = start.elapsed().as_millis() as u64;

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
fn sandbox_command(engine: &str, cwd: &str) -> Command {
    let mut cmd = Command::new(engine);
    cmd.args(sandbox_run_args(cwd))
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
/// stays visible to `kill -0`). Completed output is collected concurrently
/// so a chatty payload cannot fill a pipe and deadlock the wait.
pub(crate) async fn run_child_with_timeout(
    mut child: tokio::process::Child,
    payload: &str,
    limit: Duration,
) -> std::io::Result<RunOutcome> {
    if let Some(mut stdin) = child.stdin.take() {
        use tokio::io::AsyncWriteExt;
        let bytes = payload.as_bytes();
        if stdin.write_all(bytes).await.is_ok() && !bytes.ends_with(b"\n") {
            let _ = stdin.write_all(b"\n").await;
        }
        let _ = stdin.shutdown().await;
    }

    let stdout_task = tokio::spawn(read_pipe(child.stdout.take()));
    let stderr_task = tokio::spawn(read_pipe(child.stderr.take()));

    match timeout(limit, child.wait()).await {
        Ok(status) => {
            let status = status?;
            let stdout = stdout_task.await.unwrap_or_default();
            let stderr = stderr_task.await.unwrap_or_default();
            Ok(RunOutcome::Completed(std::process::Output {
                status,
                stdout,
                stderr,
            }))
        }
        Err(_) => {
            if let Err(e) = child.start_kill() {
                warn!("Sandbox timeout: failed to kill child: {}", e);
            }
            // Reap the killed child so no zombie survives the verdict.
            if let Err(e) = child.wait().await {
                warn!("Sandbox timeout: failed to reap child: {}", e);
            }
            stdout_task.abort();
            stderr_task.abort();
            Ok(RunOutcome::TimedOut)
        }
    }
}

async fn read_pipe<R>(pipe: Option<R>) -> Vec<u8>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut buf = Vec::new();
    if let Some(mut pipe) = pipe {
        use tokio::io::AsyncReadExt;
        let _ = pipe.read_to_end(&mut buf).await;
    }
    buf
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
    fn a_project_working_directory_is_allowed() {
        for cwd in [
            "/work/project",
            "/Users/neo/src/project",
            "/homes/project",
            "/var/runner",
            "/devices/project",
        ] {
            assert_eq!(
                working_directory_refusal(cwd),
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
