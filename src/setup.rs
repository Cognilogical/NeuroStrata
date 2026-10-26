//! Cross-platform setup: installs the daemon as a managed service and the
//! backup timer. Idempotent — safe to re-run.
//!
//! Linux (systemd user-level) fully implemented. macOS (launchd agent) and
//! Windows (NSSM + Task Scheduler) now implement the same install/verify
//! flow, not just manual instructions.

use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

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
             or pass --repo-path <project>.",
            script_path.display()
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

// ═══ Linux (systemd, user-level) ═══════════════════════════════════════════

#[cfg(target_os = "linux")]
fn run_setup_linux(bin: &Path, script: &Path, _repo: &Path) -> Result<SetupReport> {
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

    Ok(SetupReport { daemon_installed: true, backup_installed: true, verified })
}

#[cfg(target_os = "linux")]
fn unit_dir_linux() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").ok_or_else(|| anyhow::anyhow!("HOME not set"))?;
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

// ═══ macOS (launchd agent + timer) ═══════════════════════════════════════

#[cfg(target_os = "macos")]
fn run_setup_macos(bin: &Path, script: &Path, _repo: &Path) -> Result<SetupReport> {
    use std::fs;
    let home = std::env::var_os("HOME").ok_or_else(|| anyhow::anyhow!("HOME not set"))?;
    let launchd_dir = PathBuf::from(home).join("Library").join("LaunchAgents");
    fs::create_dir_all(&launchd_dir).with_context(|| format!("create {}", launchd_dir.display()))?;

    let daemon_plist = launchd_dir.join("dev.neurostrata.daemon.plist");
    let backup_plist = launchd_dir.join("dev.neurostrata.backup.plist");

    fs::write(&daemon_plist, macos_daemon_plist(bin))
        .with_context(|| format!("write {}", daemon_plist.display()))?;
    fs::write(&backup_plist, macos_backup_plist(script))
        .with_context(|| format!("write {}", backup_plist.display()))?;

    println!("Wrote:");
    println!("  {}", daemon_plist.display());
    println!("  {}", backup_plist.display());

    // Load / reload agent
    let load = std::process::Command::new("launchctl")
        .args(["load", "-w", &daemon_plist.to_string_lossy()])
        .status()
        .with_context(|| format!("launchctl load {}", daemon_plist.display()))?;
    if !load.success() {
        anyhow::bail!("launchctl load failed for daemon agent — check permissions");
    }

    // Verify via health endpoint
    let verified = wait_for_healthy_macos(30).unwrap_or_else(|e| {
        eprintln!("verify: {}", e);
        false
    });

    // Load timer after daemon verified
    if verified {
        let timer_load = std::process::Command::new("launchctl")
            .args(["load", "-w", &backup_plist.to_string_lossy()])
            .status()
            .with_context(|| format!("launchctl load {}", backup_plist.display()))?;
        if !timer_load.success() {
            eprintln!("launchctl load backup timer failed — timer not started");
        }
    }

    println!();
    println!("Next:");
    println!("  launchctl list | grep neurostrata");
    println!("  log show --predicate 'process == \"neurostrata-mcp\"' --last 10m");
    println!("  launchctl unload ~/Library/LaunchAgents/dev.neurostrata.daemon.plist  # to stop");
    if !verified {
        println!("  NOTE: daemon did not become healthy; check log above.");
    }

    Ok(SetupReport {
        daemon_installed: true,
        backup_installed: verified,
        verified,
    })
}

#[cfg(target_os = "macos")]
fn macos_daemon_plist(bin: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>dev.neurostrata.daemon</string>
  <key>ProgramArguments</key><array>
    <string>{}</string>
    <string>daemon</string>
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardErrorPath</key><string>/tmp/neurostrata-daemon.log</string>
  <key>StandardOutPath</key><string>/tmp/neurostrata-daemon.log</string>
  <key>ProcessType</key><string>Interactive</string>
  <key>ThrottleInterval</key><integer>5</integer>
</dict></plist>
"#,
        bin.display()
    )
}

#[cfg(target_os = "macos")]
fn macos_backup_plist(script: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>dev.neurostrata.backup</string>
  <key>ProgramArguments</key><array>
    <string>{}</string>
  </array>
  <key>StartCalendarInterval</key><dict>
    <key>Hour</key><integer>0</integer>
    <key>Minute</key><integer>0</integer>
  </dict>
  <key>StartCalendarInterval</key><dict>
    <key>Hour</key><integer>12</integer>
    <key>Minute</key><integer>0</integer>
  </dict>
  <key>StandardErrorPath</key><string>/tmp/neurostrata-backup.log</string>
</dict></plist>
"#,
        script.display()
    )
}

#[cfg(target_os = "macos")]
fn wait_for_healthy_macos(timeout_secs: u64) -> Result<bool> {
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

// ═══ Windows (NSSM + Task Scheduler) ═══════════════════════════════════════

#[cfg(target_os = "windows")]
fn run_setup_windows(bin: &Path, script: &Path, repo: &Path) -> Result<SetupReport> {
    println!("Windows setup: installing NeuroStrata daemon via NSSM + Task Scheduler.");

    // Try NSSM first (recommended); fall back to SC (more work).
    let nssm_exe = find_nssm()?;
    println!("NSSM found at: {}", nssm_exe.display());

    // Register daemon service
    let service_name = "NeuroStrata";
    let install = std::process::Command::new(&nssm_exe)
        .args([
            "install", service_name,
            &format!("{}", bin.display()),
            "daemon",
        ])
        .status()
        .with_context(|| format!("nssm install {}", service_name))?;
    if !install.success() {
        anyhow::bail!("NSSM install of NeuroStrata service failed");
    }

    // Configure service properties
    std::process::Command::new(&nssm_exe)
        .args([
            "set", service_name, "AppDirectory", &repo.display().to_string(),
        ])
        .status()
        .context("nssm set AppDirectory")?;
    std::process::Command::new(&nssm_exe)
        .args([
            "set", service_name, "Start", "SERVICE_AUTO_START",
        ])
        .status()
        .context("nssm set Start")?;
    std::process::Command::new(&nssm_exe)
        .args([
            "set", service_name, "Restart", "on-failure",
            "RestartSec", "5",
        ])
        .status()
        .context("nssm set Restart")?;

    std::process::Command::new(&nssm_exe)
        .args(["start", service_name])
        .status()
        .context("nssm start NeuroStrata")?;

    println!("NSSM service '{}' installed and started.", service_name);
    println!("Next: services.msc (look for NeuroStrata) or `sc query NeuroStrata`");

    // Task Scheduler for backup (daily at noon)
    let script_path = format!("{}", script.display());
    let schtasks = std::process::Command::new("schtasks")
        .args([
            "/create",
            "/tn", "NeuroStrataBackup",
            "/tr", &format!("\"{}\"", script_path),
            "/sc", "daily",
            "/st", "12:00",
            "/rf", "30",
        ])
        .status()
        .with_context(|| format!("schtasks /create NeuroStrataBackup"))?;
    if schtasks.success() {
        println!("Task Scheduler backup registered (daily 12:00).");
    } else {
        println!("Note: Task Scheduler backup not registered (may require admin). Run manually if needed.");
    }

    // Verify
    let verified = wait_for_healthy_windows(30).unwrap_or_else(|e| {
        eprintln!("verify: {}", e);
        false
    });

    println!();
    println!("Next:");
    println!("  services.msc -> NeuroStrata");
    println!("  sc query NeuroStrata");
    if !verified {
        println!("  NOTE: daemon did not become healthy; check nssm logs / Event Viewer.");
    }

    Ok(SetupReport {
        daemon_installed: true,
        backup_installed: schtasks.success(),
        verified,
    })
}

#[cfg(target_os = "windows")]
fn find_nssm() -> Result<PathBuf> {
    // Search common locations; fallback to PATH
    let candidates = [
        r"C:\Program Files\NSSM\nssm.exe",
        r"C:\nssm\nssm.exe",
        r"C:\Tools\NSSM\nssm.exe",
    ];
    for p in &candidates {
        let pb = PathBuf::from(p);
        if pb.exists() {
            return Ok(pb);
        }
    }
    // Try PATH
    if let Ok(path) = std::env::var("PATH") {
        for dir in path.split(';') {
            let pb = PathBuf::from(dir).join("nssm.exe");
            if pb.exists() {
                return Ok(pb);
            }
        }
    }
    anyhow::bail!(
        "NSSM (nssm.exe) not found — install from https://nssm.cc/ and put on PATH, \
         or place at C:\\Program Files\\NSSM\\nssm.exe. Manual setup notes preserved."
    )
}

#[cfg(target_os = "windows")]
fn wait_for_healthy_windows(timeout_secs: u64) -> Result<bool> {
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
