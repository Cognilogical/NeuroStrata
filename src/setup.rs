//! Cross-platform setup: installs the daemon as a managed service and the
//! backup timer. Idempotent — safe to re-run.
//!
//! Currently implements Linux (systemd user-level). macOS (launchd) and
//! Windows (NSSM/SCM + Task Scheduler) print manual setup instructions
//! for now; the dispatch and templates are structured so the other
//! platforms are straightforward to add.

use std::path::{Path, PathBuf};
use anyhow::{Context, Result};

/// Result of a successful setup run.
pub struct SetupReport {
    pub daemon_installed: bool,
    pub backup_installed: bool,
    pub verified: bool,
}

/// Entry point. Dispatches on platform.
pub fn run_setup(repo_path: &Path) -> Result<SetupReport> {
    let bin_path = std::env::current_exe().context("cannot determine current_exe")?;
    let script_path = repo_path.join("scripts").join("backup.sh");

    if !script_path.exists() {
        anyhow::bail!(
            "backup script not found at {}. Run from the NeuroStrata project root, \
             or pass --repo-path <project>.", script_path.display()
        );
    }

    #[cfg(target_os = "linux")]
    {
        run_setup_linux(&bin_path, &script_path, repo_path)
    }

    #[cfg(target_os = "macos")]
    {
        run_setup_macos(&bin_path, &script_path, repo_path)
    }

    #[cfg(target_os = "windows")]
    {
        run_setup_windows(&bin_path, &script_path, repo_path)
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        anyhow::bail!("unsupported platform; manual setup required")
    }
}

// ── Linux (systemd, user-level) ─────────────────────────────────────────────

#[cfg(target_os = "linux")]
fn run_setup_linux(bin: &Path, script: &Path, _repo: &Path) -> Result<SetupReport> {
    use std::fs;

    let unit_dir = unit_dir_linux()?;
    fs::create_dir_all(&unit_dir).with_context(|| format!("create {}", unit_dir.display()))?;

    let daemon_unit = unit_dir.join("neurostrata.service");
    let backup_service = unit_dir.join("neurostrata-backup.service");
    let backup_timer = unit_dir.join("neurostrata-backup.timer");

    fs::write(&daemon_unit, daemon_unit_template(bin))
        .with_context(|| format!("write {}", daemon_unit.display()))?;
    fs::write(&backup_service, backup_service_template(script))
        .with_context(|| format!("write {}", backup_service.display()))?;
    fs::write(&backup_timer, backup_timer_template())
        .with_context(|| format!("write {}", backup_timer.display()))?;

    println!("Wrote:");
    println!("  {}", daemon_unit.display());
    println!("  {}", backup_service.display());
    println!("  {}", backup_timer.display());

    run_systemctl(&["daemon-reload"])?;
    run_systemctl(&["enable", "--now", "neurostrata.service"])?;
    // Wait for the daemon to be healthy BEFORE enabling the timer. Otherwise
    // the timer's Persistent=true catch-up run can fire while the daemon is
    // still loading the embedder model, and the backup script's stop/start
    // cycle races the init.
    let verified = wait_for_healthy_linux(30).unwrap_or_else(|e| {
        eprintln!("verify: {}", e);
        false
    });
    if verified {
        run_systemctl(&["enable", "--now", "neurostrata-backup.timer"])?;
    } else {
        eprintln!("Skipping backup timer: daemon not healthy. Run `neurostrata-mcp setup` again once it's up.");
    }

    println!();
    println!("Next:");
    println!("  systemctl --user status neurostrata.service");
    println!("  systemctl --user list-timers neurostrata-backup.timer");
    println!("  journalctl --user -u neurostrata -n 50");
    if !verified {
        println!("  NOTE: daemon did not become healthy; check journal above.");
    }

    Ok(SetupReport {
        daemon_installed: true,
        backup_installed: true,
        verified,
    })
}

#[cfg(target_os = "linux")]
fn unit_dir_linux() -> Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .ok_or_else(|| anyhow::anyhow!("HOME not set"))?;
    let mut p = PathBuf::from(home);
    p.push(".config");
    p.push("systemd");
    p.push("user");
    Ok(p)
}

#[cfg(target_os = "linux")]
fn run_systemctl(args: &[&str]) -> Result<()> {
    let status = std::process::Command::new("systemctl")
        .arg("--user")
        .args(args)
        .status()
        .with_context(|| format!("systemctl --user {}", args.join(" ")))?;
    if !status.success() {
        anyhow::bail!("systemctl --user {} exited with {:?}", args.join(" "), status.code());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn verify_daemon_linux() -> Result<bool> {
    let port = std::env::var("NEUROSTRATA_DAEMON_PORT").unwrap_or_else(|_| "34343".to_string());
    let url = format!("http://127.0.0.1:{}/health", port);
    let status = std::process::Command::new("curl")
        .args(["-s", "-f", &url])
        .status()
        .context("curl health check")?;
    Ok(status.success())
}

#[cfg(target_os = "linux")]
fn wait_for_healthy_linux(timeout_secs: u64) -> Result<bool> {
    let port = std::env::var("NEUROSTRATA_DAEMON_PORT").unwrap_or_else(|_| "34343".to_string());
    let url = format!("http://127.0.0.1:{}/health", port);
    for _ in 0..timeout_secs {
        if let Ok(status) = std::process::Command::new("curl")
            .args(["-s", "-f", &url])
            .status()
        {
            if status.success() {
                return Ok(true);
            }
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    anyhow::bail!("daemon did not become healthy within {}s", timeout_secs)
}

fn daemon_unit_template(bin: &Path) -> String {
    format!(
        "[Unit]\n\
         Description=NeuroStrata MCP daemon\n\
         After=default.target\n\
         \n\
         [Service]\n\
         Type=simple\n\
         ExecStart={} daemon\n\
         Restart=on-failure\n\
         RestartSec=5\n\
         Nice=10\n\
         # Logs: journalctl --user -u neurostrata -n 50\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        bin.display()
    )
}

fn backup_service_template(script: &Path) -> String {
    format!(
        "[Unit]\n\
         Description=NeuroStrata DB backup (CHECKPOINT+cp)\n\
         After=network.target\n\
         \n\
         [Service]\n\
         Type=oneshot\n\
         ExecStart={}\n\
         Nice=10\n\
         # Logs: journalctl --user -u neurostrata-backup -n 50\n",
        script.display()
    )
}

fn backup_timer_template() -> String {
    // Noon and midnight; Persistent=true catches missed runs.
    "[Unit]\n\
     Description=Schedule NeuroStrata DB backup at noon and midnight\n\
     Requires=neurostrata-backup.service\n\
     \n\
     [Timer]\n\
     OnCalendar=*-*-* 00,12:00:00\n\
     Persistent=true\n\
     RandomizedDelaySec=60\n\
     \n\
     [Install]\n\
     WantedBy=timers.target\n"
        .to_string()
}

// ── macOS (launchd) ─────────────────────────────────────────────────────────

#[cfg(target_os = "macos")]
fn run_setup_macos(_bin: &Path, _script: &Path, _repo: &Path) -> Result<SetupReport> {
    println!("macOS support: not yet implemented. Manual setup:\n");
    println!("  1. Copy scripts/backup.sh to a stable location:");
    println!("       mkdir -p ~/.local/share/neurostrata/scripts");
    println!("       cp scripts/backup.sh ~/.local/share/neurostrata/scripts/");
    println!("  2. Install LaunchAgent for the daemon (~/Library/LaunchAgents/dev.neurostrata.daemon.plist):");
    println!("       <?xml version=\"1.0\" encoding=\"UTF-8\"?>");
    println!("       <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">");
    println!("       <plist version=\"1.0\"><dict>");
    println!("         <key>Label</key><string>dev.neurostrata.daemon</string>");
    println!("         <key>ProgramArguments</key><array>");
    println!("           <string>{}</string>", _bin.display());
    println!("           <string>daemon</string>");
    println!("         </array>");
    println!("         <key>RunAtLoad</key><true/>");
    println!("         <key>KeepAlive</key><true/>");
    println!("       </dict></plist>");
    println!("  3. Load the agent:");
    println!("       launchctl load -w ~/Library/LaunchAgents/dev.neurostrata.daemon.plist");
    println!("  4. For backups, add StartCalendarInterval to the same plist or a separate one.");
    Ok(SetupReport { daemon_installed: false, backup_installed: false, verified: false })
}

// ── Windows (NSSM / SCM) ───────────────────────────────────────────────────

#[cfg(target_os = "windows")]
fn run_setup_windows(_bin: &Path, _script: &Path, _repo: &Path) -> Result<SetupReport> {
    println!("Windows support: not yet implemented. Manual setup options:\n");
    println!("  Option A — NSSM (recommended):");
    println!("    1. Install NSSM (https://nssm.cc/) and put nssm.exe on PATH.");
    println!("    2. Register the daemon:");
    println!("         nssm install NeuroStrata \"{}\" daemon", _bin.display());
    println!("         nssm set NeuroStrata AppDirectory \"{}\"", _repo.display());
    println!("         nssm set NeuroStrata Start SERVICE_AUTO_START");
    println!("         nssm start NeuroStrata");
    println!("    3. For backups, use Task Scheduler:");
    println!("         schtasks /create /tn NeuroStrataBackup /tr \"{}\" /sc daily /st 12:00", _script.display());
    println!("  Option B — SCM directly (no NSSM): more work; NSSM is simpler.");
    Ok(SetupReport { daemon_installed: false, backup_installed: false, verified: false })
}
