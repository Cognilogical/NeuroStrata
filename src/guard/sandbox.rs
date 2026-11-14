use crate::guard::models::ValidateVerdict;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::timeout;
use tracing::{info, warn};

/// R26: the sandbox container image is pinned to a specific Alpine release
/// (instead of `latest`) so a registry re-push cannot silently change the
/// sandbox behavior.
pub const SANDBOX_IMAGE: &str = "alpine:3.21";

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
    engine: String, // "podman" or "docker"
    available: bool,
}

impl SandboxEvaluator {
    pub async fn new() -> Self {
        let (engine, available) = Self::detect_engine().await;
        Self { engine, available }
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
        // Re-probe dynamically: `available` was computed once in the
        // constructor, so Podman may have appeared (or gone) since. Probing
        // again here keeps this check from contradicting the daemon's own
        // live probe at the request boundary (TOCTOU).
        let mut engine = self.engine.clone();
        let mut available = self.available;
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

        let forbidden_prefixes = [
            "/etc", "/var/run", "/root", "/sys", "/dev", "/proc", "/boot",
        ];
        if cwd_str == "/"
            || forbidden_prefixes
                .iter()
                .any(|prefix| cwd_str.starts_with(prefix))
        {
            return Ok(ValidateVerdict::DeterministicReject {
                reasons: vec![format!(
                    "Security violation: Attempted to mount forbidden host path {}",
                    cwd_str
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

        let start = std::time::Instant::now();

        // Command execution inside ephemeral container
        // --rm : ephemeral
        // -i / sh -s : stream payload via stdin (never pass untrusted payload in argv)
        // --network=none : isolate
        // -v cwd:/workspace:ro : mount project read-only
        // --tmpfs /tmp : scratch space
        let mount_arg = format!("-v={}:/workspace:ro", cwd_str);
        let child = Command::new(&engine)
            .args(sandbox_run_args(&mount_arg))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Kill the container when the child future is dropped (the
            // timeout path): an abandoned `podman run` would otherwise keep
            // executing after the verdict has moved on.
            .kill_on_drop(true)
            .spawn()?;

        // Wait with a strict timeout (e.g., 5 seconds for inline guard)
        let execution_result = timeout(
            Duration::from_secs(5),
            write_payload_and_wait(child, payload),
        )
        .await;

        let duration_ms = start.elapsed().as_millis() as u64;

        match execution_result {
            Ok(Ok(output)) => {
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
            Ok(Err(e)) => {
                warn!("Sandbox execution failed to run: {}", e);
                Ok(ValidateVerdict::SandboxReject {
                    reasons: vec![format!("Sandbox execution error: {}", e)],
                    logs: String::new(),
                    constraints: vec![],
                })
            }
            Err(_) => {
                // Timeout
                warn!("Sandbox execution timed out after 5 seconds");
                Ok(ValidateVerdict::SandboxReject {
                    reasons: vec!["Sandbox timed out".to_string()],
                    logs: String::new(),
                    constraints: vec![],
                })
            }
        }
    }
}

/// Fixed CLI arguments for `podman run`. The shell payload is piped over
/// `stdin` to `sh -s` rather than passed via `-c <payload>` on the process
/// command line, preventing command-line injection (`rust/command-line-injection`)
/// and keeping untrusted payloads out of host `argv` / `/proc/<pid>/cmdline`.
fn sandbox_run_args(mount_arg: &str) -> [&str; 10] {
    [
        "run",
        "--rm",
        "-i",
        "--network=none",
        mount_arg,
        "--tmpfs=/tmp",
        "-w=/workspace",
        SANDBOX_IMAGE,
        "sh",
        "-s",
    ]
}

async fn write_payload_and_wait(
    mut child: tokio::process::Child,
    payload: &str,
) -> std::io::Result<std::process::Output> {
    if let Some(mut stdin) = child.stdin.take() {
        use tokio::io::AsyncWriteExt;
        let bytes = payload.as_bytes();
        if stdin.write_all(bytes).await.is_ok() && !bytes.ends_with(b"\n") {
            let _ = stdin.write_all(b"\n").await;
        }
        let _ = stdin.shutdown().await;
    }
    child.wait_with_output().await
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
    fn sandbox_run_args_streams_script_over_stdin_without_cli_payload() {
        let args = sandbox_run_args("-v=/tmp/ws:/workspace:ro");
        assert_eq!(
            args,
            [
                "run",
                "--rm",
                "-i",
                "--network=none",
                "-v=/tmp/ws:/workspace:ro",
                "--tmpfs=/tmp",
                "-w=/workspace",
                SANDBOX_IMAGE,
                "sh",
                "-s",
            ]
        );
        assert!(
            !args.contains(&"-c"),
            "sandbox must read payload from stdin via `sh -s`, never `sh -c <payload>`"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn write_payload_and_wait_streams_to_sh_stdin_and_preserves_status() {
        let ok_child = Command::new("sh")
            .arg("-s")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn sh -s");
        let ok_out = write_payload_and_wait(ok_child, "printf 'sandbox-ok'")
            .await
            .expect("wait sh -s");
        assert!(ok_out.status.success());
        assert_eq!(String::from_utf8_lossy(&ok_out.stdout), "sandbox-ok");

        let fail_child = Command::new("sh")
            .arg("-s")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn failing sh -s");
        let fail_out = write_payload_and_wait(fail_child, "echo 'bad' >&2\nexit 7")
            .await
            .expect("wait failing sh -s");
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
