use crate::guard::models::ValidateVerdict;
use tracing::{info, warn};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::timeout;

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
        if !self.available {
            return Ok(ValidateVerdict::DeterministicReject {
                reasons: vec!["No container engine (Podman) available. Sandbox evaluation required. Please install Podman for full security.".to_string()],
                constraints: vec![],
            });
        }

        // Validate and canonicalize cwd to prevent host path exfiltration
        let canonical_cwd = std::fs::canonicalize(cwd).map_err(|_| anyhow::anyhow!("Invalid cwd path"))?;
        let cwd_str = canonical_cwd.to_string_lossy().to_string();

        let forbidden_prefixes = ["/etc", "/var/run", "/root", "/sys", "/dev", "/proc", "/boot"];
        if cwd_str == "/" || forbidden_prefixes.iter().any(|prefix| cwd_str.starts_with(prefix)) {
            return Ok(ValidateVerdict::DeterministicReject {
                reasons: vec![format!("Security violation: Attempted to mount forbidden host path {}", cwd_str)],
                constraints: vec!["Never attempt to interact with or mount sensitive host system paths.".to_string()],
            });
        }

        // We only sandbox bash or known script commands for now
        if action_type != "bash" && action_type != "script" {
            return Ok(ValidateVerdict::DeterministicReject {
                reasons: vec![format!("Sandbox evaluation not supported for action_type: {}", action_type)],
                constraints: vec![],
            });
        }

        info!("Executing sandbox evaluation using engine: {}", self.engine);

        let start = std::time::Instant::now();

        // Command execution inside ephemeral container
        // --rm : ephemeral
        // --network=none : isolate
        // -v cwd:/workspace:ro : mount project read-only
        // --tmpfs /tmp : scratch space
        let child = Command::new(&self.engine)
            .args([
                "run",
                "--rm",
                "--network=none",
                &format!("-v={}:/workspace:ro", cwd_str),
                "--tmpfs=/tmp",
                "-w=/workspace",
                "alpine:latest",
                "sh",
                "-c",
                payload,
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        // Wait with a strict timeout (e.g., 5 seconds for inline guard)
        let execution_result = timeout(Duration::from_secs(5), child.wait_with_output()).await;

        let duration_ms = start.elapsed().as_millis() as u64;

        match execution_result {
            Ok(Ok(output)) => {
                let mut stdout = String::from_utf8_lossy(&output.stdout).into_owned();
                let mut stderr = String::from_utf8_lossy(&output.stderr).into_owned();

                // Truncate logs to prevent LLM context overflow which causes death spirals and compaction errors
                if stdout.len() > 500 {
                    stdout = format!("...[TRUNCATED {} chars]...\n{}", stdout.len() - 500, &stdout[stdout.len() - 500..]);
                }
                if stderr.len() > 500 {
                    stderr = format!("...[TRUNCATED {} chars]...\n{}", stderr.len() - 500, &stderr[stderr.len() - 500..]);
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
