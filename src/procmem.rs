//! The daemon's own resident memory, and a log of which tool calls grew it.
//!
//! One daemon serves every console, so its memory is the sum of everything
//! every client has asked of it. `neurostrata_storage_status` reports the
//! current and peak figures; `CallGuard` wraps one MCP tool call and appends a
//! JSON line to `memory-growth.log` when the call left the process at least
//! `NEUROSTRATA_RSS_LOG_MB` (default 50) larger than it found it. After a day
//! of normal use that file names the calls that drive growth.
//!
//! Calls run concurrently, so a line records `in_flight`: when it is above 1,
//! the growth may belong to a neighboring call and the line says
//! `"approximate": true`.
//!
//! Opt-in: the log and the sampler run only with `neurostrata-mcp daemon
//! --profile` or `NEUROSTRATA_PROFILE=1`. The resident figure in
//! `storage_status` is always on.

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

const MB: u64 = 1024 * 1024;
const DEFAULT_THRESHOLD_MB: u64 = 50;
/// The log is rewritten from empty once it passes this size, keeping the
/// previous file as `memory-growth.log.1`.
const MAX_LOG_BYTES: u64 = 4 * MB;

/// Seconds between time-series rows when the environment does not say.
const DEFAULT_SAMPLE_SECS: u64 = 600;

static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);
/// Tool calls begun since the last time-series row.
static CALLS: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Sample {
    /// Resident set now. `None` where the platform gives no current figure.
    pub rss_bytes: Option<u64>,
    /// Highest resident set since the process started.
    pub peak_bytes: Option<u64>,
}

#[cfg(windows)]
pub fn sample() -> Sample {
    #[repr(C)]
    struct ProcessMemoryCounters {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcess() -> isize;
        fn K32GetProcessMemoryInfo(
            process: isize,
            counters: *mut ProcessMemoryCounters,
            cb: u32,
        ) -> i32;
    }
    let mut counters = ProcessMemoryCounters {
        cb: std::mem::size_of::<ProcessMemoryCounters>() as u32,
        page_fault_count: 0,
        peak_working_set_size: 0,
        working_set_size: 0,
        quota_peak_paged_pool_usage: 0,
        quota_paged_pool_usage: 0,
        quota_peak_non_paged_pool_usage: 0,
        quota_non_paged_pool_usage: 0,
        pagefile_usage: 0,
        peak_pagefile_usage: 0,
    };
    // SAFETY: GetCurrentProcess returns a pseudo-handle that needs no closing,
    // and `counters` is a correctly sized, writable struct whose `cb` says so.
    let ok = unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb) };
    if ok == 0 {
        return Sample::default();
    }
    Sample {
        rss_bytes: Some(counters.working_set_size as u64),
        peak_bytes: Some(counters.peak_working_set_size as u64),
    }
}

#[cfg(target_os = "linux")]
pub fn sample() -> Sample {
    let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
        return Sample::default();
    };
    let kb = |key: &str| -> Option<u64> {
        status
            .lines()
            .find_map(|l| l.strip_prefix(key))
            .and_then(|rest| rest.trim().trim_end_matches("kB").trim().parse::<u64>().ok())
            .map(|v| v * 1024)
    };
    Sample { rss_bytes: kb("VmRSS:"), peak_bytes: kb("VmHWM:") }
}

#[cfg(all(unix, not(target_os = "linux")))]
pub fn sample() -> Sample {
    // No current figure without platform-specific calls; the peak is what
    // getrusage offers. ru_maxrss is bytes on macOS and kilobytes elsewhere.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: `usage` is a valid, writable rusage.
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) } != 0 {
        return Sample::default();
    }
    let raw = usage.ru_maxrss as u64;
    let peak = if cfg!(target_os = "macos") { raw } else { raw * 1024 };
    Sample { rss_bytes: None, peak_bytes: Some(peak) }
}

#[cfg(not(any(windows, unix)))]
pub fn sample() -> Sample {
    Sample::default()
}

fn mb(bytes: Option<u64>) -> serde_json::Value {
    bytes.map_or(serde_json::Value::Null, |b| serde_json::json!(b / MB))
}

/// Marker the daemon puts in an error when the database buffer pool is full.
/// Harness plugins look for it to remind the agent to tell the user.
pub const ADVICE_MARKER_BUFFER_POOL: &str = "NEUROSTRATA_ADVICE[buffer_pool]";

/// Resident memory (MB) at which the daemon starts to mention it. Idle is about
/// 860 MB and one large search used to take it to about 1.9 GB, so these are
/// starting points; `advice` lines in the growth log show when they fire.
const DEFAULT_WATCH_MB: u64 = 1536;
/// Resident memory (MB) at which the agent is asked to raise it with the user.
const DEFAULT_ACT_MB: u64 = 2560;

/// Pool-full failures since the process started.
static POOL_FULL_ERRORS: AtomicU64 = AtomicU64::new(0);
/// The buffer pool size in force, in MB; 0 means the engine sized it.
static POOL_MB: AtomicU64 = AtomicU64::new(0);
/// The advice level last seen, so the growth log gets one line per change.
static LAST_LEVEL: AtomicUsize = AtomicUsize::new(0);

/// Called when the engine reports that its buffer pool is full.
pub fn note_pool_full() {
    POOL_FULL_ERRORS.fetch_add(1, Ordering::Relaxed);
}

/// Called once the daemon knows which pool size it is running with.
pub fn set_pool_mb(mb: u64) {
    POOL_MB.store(mb, Ordering::Relaxed);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Ok,
    Watch,
    Act,
}

impl Level {
    fn as_str(self) -> &'static str {
        match self {
            Level::Ok => "ok",
            Level::Watch => "watch",
            Level::Act => "act",
        }
    }
    fn index(self) -> usize {
        match self {
            Level::Ok => 0,
            Level::Watch => 1,
            Level::Act => 2,
        }
    }
}

fn env_mb(name: &str, default: u64) -> u64 {
    std::env::var(name).ok().and_then(|v| v.trim().parse().ok()).unwrap_or(default)
}

/// How worried to be, and why. Pure, so the thresholds are testable. A pool-full
/// failure is always `Act`: queries are failing. Memory alone is `Watch` at the
/// first threshold and `Act` at the second. An unknown figure is never a reason.
pub fn assess(rss_mb: Option<u64>, pool_full_errors: u64, watch_mb: u64, act_mb: u64) -> (Level, String) {
    if pool_full_errors > 0 {
        return (
            Level::Act,
            format!("the database buffer pool filled up {pool_full_errors} time(s) since the daemon started, and the query that hit it failed"),
        );
    }
    match rss_mb {
        Some(r) if r >= act_mb => (Level::Act, format!("the daemon is using {r} MB of memory (act threshold {act_mb} MB)")),
        Some(r) if r >= watch_mb => (Level::Watch, format!("the daemon is using {r} MB of memory (watch threshold {watch_mb} MB)")),
        _ => (Level::Ok, String::new()),
    }
}

/// What the agent should do at this level. Empty when nothing is wrong.
fn suggestions(level: Level, pool_full_errors: u64, pool_mb: u64) -> Vec<String> {
    if level == Level::Ok {
        return Vec::new();
    }
    let mut out = vec!["Tell the user what you saw and ask before doing anything about it.".to_string()];
    if pool_full_errors > 0 {
        let now = if pool_mb == 0 { "sized by the engine".to_string() } else { format!("{pool_mb} MB") };
        out.push(format!(
            "Raise storage_hygiene.buffer_pool_mb in ~/.config/neurostrata/config.json (now {now}) or set NEUROSTRATA_BUFFER_POOL_MB; it takes effect when the daemon restarts."
        ));
    } else if level == Level::Act {
        out.push(
            "If it keeps growing, the buffer pool cap can be lowered (storage_hygiene.buffer_pool_mb, or NEUROSTRATA_BUFFER_POOL_MB) at some cost in query speed; a restart frees the memory now."
                .to_string(),
        );
    }
    out.push(
        "Restarting is the user's to do: `neurostrata-mcp shutdown` (graceful, can take minutes), then the next MCP connection starts a new daemon. It disconnects every IDE that shares the daemon."
            .to_string(),
    );
    out.push("Never kill the daemon process: memories not yet checkpointed are lost.".to_string());
    out
}

/// The `advice` object in `neurostrata_storage_status`. Also writes one `advice`
/// line to the growth log whenever the level changes, so the thresholds can be
/// tuned from real use.
fn advice_json(s: Sample) -> serde_json::Value {
    let rss = s.rss_bytes.map(|b| b / MB);
    let (watch, act) = (env_mb("NEUROSTRATA_RSS_WATCH_MB", DEFAULT_WATCH_MB), env_mb("NEUROSTRATA_RSS_ACT_MB", DEFAULT_ACT_MB));
    let pool_full = POOL_FULL_ERRORS.load(Ordering::Relaxed);
    let pool_mb = POOL_MB.load(Ordering::Relaxed);
    let (level, reason) = assess(rss, pool_full, watch, act);
    let previous = LAST_LEVEL.swap(level.index(), Ordering::Relaxed);
    // Not in tests: they must not write into the real growth log.
    if !cfg!(test) && previous != level.index() && profile_enabled() {
        append(
            &log_path(),
            &serde_json::json!({
                "kind": "advice",
                "ts": now_secs(),
                "level": level.as_str(),
                "reason": reason,
                "rss_mb": rss,
                "pool_full_errors": pool_full,
            })
            .to_string(),
        );
    }
    serde_json::json!({
        "level": level.as_str(),
        "reason": reason,
        "pool_full_errors": pool_full,
        "buffer_pool_mb": pool_mb,
        "watch_mb": watch,
        "act_mb": act,
        "suggestions": suggestions(level, pool_full, pool_mb),
    })
}

/// A short block for `neurostrata_get_snapshot`, which agents read first, or
/// `None` when memory is fine.
pub fn snapshot_notice() -> Option<String> {
    let advice = advice_json(sample());
    let level = advice["level"].as_str().unwrap_or("ok");
    if level == "ok" {
        return None;
    }
    let reason = advice["reason"].as_str().unwrap_or("");
    let mut out = format!("DAEMON MEMORY ADVISORY ({level}): {reason}.");
    if level == "watch" {
        out.push_str(" Nothing is failing. Mention it to the user if it matters to what they are doing.");
        return Some(out);
    }
    for (i, step) in advice["suggestions"].as_array().into_iter().flatten().filter_map(|v| v.as_str()).enumerate() {
        out.push_str(&format!("\n  {}. {}", i + 1, step));
    }
    Some(out)
}

/// The figures `neurostrata_storage_status` reports. Null where the platform
/// has none, never a made-up zero.
pub fn status_json() -> serde_json::Value {
    let s = sample();
    serde_json::json!({
        "rss_mb": mb(s.rss_bytes),
        "peak_rss_mb": mb(s.peak_bytes),
        "growth_log": log_path().to_string_lossy(),
        "growth_log_threshold_mb": threshold_mb(),
        "advice": advice_json(s),
    })
}

/// The storage status plus a `process` object: the daemon's resident and peak
/// memory, and where the per-call growth log is written.
pub fn status_with_process_memory<T: serde::Serialize>(status: &T) -> String {
    let mut value = match serde_json::to_value(status) {
        Ok(v) => v,
        Err(e) => return serde_json::json!({"ok": false, "error": e.to_string()}).to_string(),
    };
    if let Some(obj) = value.as_object_mut() {
        obj.insert("process".to_string(), status_json());
    }
    value.to_string()
}

/// Set by `neurostrata-mcp daemon --profile`.
static PROFILE: AtomicBool = AtomicBool::new(false);

/// Turns profiling on for this process: the growth log and the sampler.
pub fn enable_profile() {
    PROFILE.store(true, Ordering::Relaxed);
}

/// The growth log and the sampler are opt-in: `neurostrata-mcp daemon
/// --profile`, or `NEUROSTRATA_PROFILE=1` (also `true`/`on`) for a daemon that
/// an IDE starts. The resident figure in `storage_status` stays on: it is one
/// syscall and has nothing to switch.
fn profile_enabled() -> bool {
    PROFILE.load(Ordering::Relaxed)
        || matches!(
            std::env::var("NEUROSTRATA_PROFILE").ok().as_deref().map(str::trim),
            Some("1") | Some("true") | Some("on")
        )
}

fn threshold_mb() -> u64 {
    std::env::var("NEUROSTRATA_RSS_LOG_MB")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_THRESHOLD_MB)
}

fn log_path() -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    home.join(".config").join("neurostrata").join("memory-growth.log")
}

/// True when `after` is at least `threshold_mb` above `before`. Unknown
/// figures never count as growth.
fn grew(before: Option<u64>, after: Option<u64>, threshold_mb: u64) -> bool {
    match (before, after) {
        (Some(b), Some(a)) => a.saturating_sub(b) >= threshold_mb * MB,
        _ => false,
    }
}

fn line(tool: &str, namespace: Option<&str>, before: Sample, after: Sample, in_flight: usize) -> String {
    serde_json::json!({
        "kind": "call",
        "ts": now_secs(),
        "tool": tool,
        "namespace": namespace,
        "rss_before_mb": mb(before.rss_bytes),
        "rss_after_mb": mb(after.rss_bytes),
        "delta_mb": match (before.rss_bytes, after.rss_bytes) {
            (Some(b), Some(a)) => serde_json::json!((a as i64 - b as i64) / MB as i64),
            _ => serde_json::Value::Null,
        },
        "peak_rss_mb": mb(after.peak_bytes),
        "in_flight": in_flight,
        // Process-wide RSS: a neighboring call may own part of the delta.
        "approximate": in_flight > 1,
    })
    .to_string()
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// One row of the time series: the process's memory at a moment, and how many
/// tool calls ran since the previous row. Slow growth that no single call
/// accounts for shows up here and nowhere else.
fn sample_line(s: Sample, calls_since_last: u64, in_flight: usize) -> String {
    serde_json::json!({
        "kind": "sample",
        "ts": now_secs(),
        "rss_mb": mb(s.rss_bytes),
        "peak_rss_mb": mb(s.peak_bytes),
        "calls_since_last": calls_since_last,
        "in_flight": in_flight,
    })
    .to_string()
}

fn sample_interval() -> Option<std::time::Duration> {
    let secs = std::env::var("NEUROSTRATA_RSS_SAMPLE_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_SAMPLE_SECS);
    (secs > 0).then(|| std::time::Duration::from_secs(secs))
}

/// Starts the time series: one row now, then one every
/// `NEUROSTRATA_RSS_SAMPLE_SECS` (default 600; 0 turns it off) until `stop`
/// changes. The caller awaits the returned handle on shutdown.
pub fn spawn_sampler(stop: tokio::sync::watch::Receiver<bool>) -> tokio::task::JoinHandle<()> {
    match sample_interval().filter(|_| profile_enabled()) {
        Some(every) => tokio::spawn(run_sampler(log_path(), every, stop)),
        None => tokio::spawn(async {}),
    }
}

async fn run_sampler(
    path: PathBuf,
    every: std::time::Duration,
    mut stop: tokio::sync::watch::Receiver<bool>,
) {
    loop {
        let calls = CALLS.swap(0, Ordering::Relaxed);
        append(&path, &sample_line(sample(), calls, IN_FLIGHT.load(Ordering::SeqCst)));
        // Evaluates the advice level, so a change is logged even when nobody
        // asked for the status.
        let _ = advice_json(sample());
        tokio::select! {
            _ = tokio::time::sleep(every) => {}
            _ = stop.changed() => break,
        }
    }
}

fn append(path: &std::path::Path, line: &str) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if std::fs::metadata(path).map(|m| m.len() > MAX_LOG_BYTES).unwrap_or(false) {
        let mut rolled = path.as_os_str().to_owned();
        rolled.push(".1");
        let _ = std::fs::rename(path, PathBuf::from(rolled));
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{line}");
    }
}

/// Wraps one tool call. Created before the handler runs; on drop it samples
/// again and logs the call when the process grew past the threshold.
pub struct CallGuard {
    tool: String,
    namespace: Option<String>,
    before: Sample,
}

impl CallGuard {
    pub fn begin(tool: &str, namespace: Option<&str>) -> Self {
        IN_FLIGHT.fetch_add(1, Ordering::SeqCst);
        CALLS.fetch_add(1, Ordering::Relaxed);
        Self {
            tool: tool.to_string(),
            namespace: namespace.map(str::to_string),
            before: sample(),
        }
    }
}

impl Drop for CallGuard {
    fn drop(&mut self) {
        let in_flight = IN_FLIGHT.fetch_sub(1, Ordering::SeqCst);
        let after = sample();
        if profile_enabled() && grew(self.before.rss_bytes, after.rss_bytes, threshold_mb()) {
            append(
                &log_path(),
                &line(&self.tool, self.namespace.as_deref(), self.before, after, in_flight),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn growth_needs_both_figures_and_the_threshold() {
        assert!(grew(Some(100 * MB), Some(150 * MB), 50));
        assert!(!grew(Some(100 * MB), Some(149 * MB), 50));
        assert!(!grew(Some(150 * MB), Some(100 * MB), 50), "shrinking is not growth");
        assert!(!grew(None, Some(900 * MB), 50), "an unknown start is not a measurement");
        assert!(!grew(Some(100 * MB), None, 50));
    }

    #[test]
    fn this_platform_reports_a_resident_set() {
        let s = sample();
        #[cfg(any(windows, target_os = "linux"))]
        {
            assert!(s.rss_bytes.unwrap_or(0) > 0, "no resident set reported: {s:?}");
            assert!(s.peak_bytes.unwrap_or(0) >= s.rss_bytes.unwrap_or(0));
        }
        let _ = s;
    }

    #[test]
    fn a_log_line_is_strict_json_naming_the_call() {
        let before = Sample { rss_bytes: Some(1000 * MB), peak_bytes: Some(1000 * MB) };
        let after = Sample { rss_bytes: Some(1300 * MB), peak_bytes: Some(1300 * MB) };
        let v: serde_json::Value =
            serde_json::from_str(&line("neurostrata_search_memory", Some("example-project"), before, after, 2)).unwrap();
        assert_eq!(v["tool"], "neurostrata_search_memory");
        assert_eq!(v["namespace"], "example-project");
        assert_eq!(v["delta_mb"], 300);
        assert_eq!(v["in_flight"], 2);
    }

    #[test]
    fn advice_levels_follow_memory_and_pool_failures() {
        let (w, a) = (1536, 2560);
        assert_eq!(assess(Some(860), 0, w, a).0, Level::Ok);
        assert_eq!(assess(Some(1535), 0, w, a).0, Level::Ok);
        assert_eq!(assess(Some(1536), 0, w, a).0, Level::Watch);
        assert_eq!(assess(Some(2559), 0, w, a).0, Level::Watch);
        assert_eq!(assess(Some(2560), 0, w, a).0, Level::Act);
        assert_eq!(assess(None, 0, w, a).0, Level::Ok, "an unknown figure is not a reason");
        let (level, reason) = assess(Some(500), 2, w, a);
        assert_eq!(level, Level::Act, "a failed query is always worth raising");
        assert!(reason.contains("2 time(s)"), "{reason}");
    }

    #[test]
    fn ok_has_no_suggestions_and_act_never_tells_the_agent_to_restart_it() {
        assert!(suggestions(Level::Ok, 0, 384).is_empty());
        let act = suggestions(Level::Act, 1, 384).join("\n");
        assert!(act.contains("ask before"), "{act}");
        assert!(act.contains("storage_hygiene.buffer_pool_mb"), "{act}");
        assert!(act.contains("384 MB"), "{act}");
        assert!(act.contains("neurostrata-mcp shutdown"), "{act}");
        assert!(act.contains("user's to do"), "{act}");
        assert!(act.contains("Never kill"), "{act}");
        let sized_by_engine = suggestions(Level::Act, 1, 0).join("\n");
        assert!(sized_by_engine.contains("sized by the engine"), "{sized_by_engine}");
    }

    #[test]
    fn the_status_carries_an_advice_object_that_is_explicit_when_all_is_well() {
        let out = status_with_process_memory(&serde_json::json!({"dirty": false}));
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        let advice = &v["process"]["advice"];
        assert!(advice["level"].is_string(), "{advice}");
        assert!(advice["suggestions"].is_array());
        assert!(advice["watch_mb"].as_u64().unwrap() > 0 && advice["act_mb"].as_u64().unwrap() > advice["watch_mb"].as_u64().unwrap());
    }

    #[test]
    fn a_pool_full_failure_is_counted_and_surfaces_in_the_snapshot_notice() {
        let before = POOL_FULL_ERRORS.load(Ordering::Relaxed);
        note_pool_full();
        assert_eq!(POOL_FULL_ERRORS.load(Ordering::Relaxed), before + 1);
        let notice = snapshot_notice().expect("a pool failure must produce a notice");
        assert!(notice.starts_with("DAEMON MEMORY ADVISORY (act)"), "{notice}");
        assert!(notice.contains("1. Tell the user"), "{notice}");
        assert!(ADVICE_MARKER_BUFFER_POOL.starts_with("NEUROSTRATA_ADVICE["));
    }

    #[test]
    fn overlapping_calls_are_marked_approximate() {
        let s = Sample { rss_bytes: Some(MB), peak_bytes: Some(MB) };
        let solo: serde_json::Value = serde_json::from_str(&line("t", None, s, s, 1)).unwrap();
        let busy: serde_json::Value = serde_json::from_str(&line("t", None, s, s, 2)).unwrap();
        assert_eq!(solo["approximate"], false);
        assert_eq!(busy["approximate"], true);
    }

    #[test]
    fn profiling_is_off_unless_asked_for() {
        std::env::remove_var("NEUROSTRATA_PROFILE");
        assert!(!profile_enabled(), "off by default");
        for off in ["0", "off", "false", ""] {
            std::env::set_var("NEUROSTRATA_PROFILE", off);
            assert!(!profile_enabled(), "{off:?} is not a yes");
        }
        for on in ["1", "true", "on"] {
            std::env::set_var("NEUROSTRATA_PROFILE", on);
            assert!(profile_enabled(), "{on:?} turns it on");
        }
        std::env::remove_var("NEUROSTRATA_PROFILE");
        assert!(!profile_enabled());
        // `enable_profile()` is process-wide and one-way, so it is not
        // exercised here: it would leave profiling on for every other test.
    }

    #[test]
    fn unknown_figures_are_null_not_zero() {
        let v: serde_json::Value =
            serde_json::from_str(&line("t", None, Sample::default(), Sample::default(), 1)).unwrap();
        assert!(v["rss_after_mb"].is_null());
        assert!(v["delta_mb"].is_null());
        assert!(v["namespace"].is_null());
    }

    #[test]
    fn the_log_rolls_over_instead_of_growing_without_bound() {
        let dir = std::env::temp_dir().join(format!("procmem-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("memory-growth.log");
        std::fs::write(&path, vec![b'x'; (MAX_LOG_BYTES + 1) as usize]).unwrap();
        append(&path, "{\"fresh\":true}");
        assert_eq!(std::fs::read_to_string(&path).unwrap().trim(), "{\"fresh\":true}");
        assert!(dir.join("memory-growth.log.1").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_status_keeps_its_fields_and_gains_a_process_object() {
        let out = status_with_process_memory(&serde_json::json!({"dirty": false, "snapshots": 3}));
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["dirty"], false, "existing fields are untouched");
        assert_eq!(v["snapshots"], 3);
        let p = &v["process"];
        assert!(p.get("rss_mb").is_some() && p.get("peak_rss_mb").is_some());
        assert!(p["growth_log"].as_str().unwrap().ends_with("memory-growth.log"));
        assert!(p["growth_log_threshold_mb"].as_u64().unwrap() > 0);
    }

    #[test]
    fn a_sample_row_is_strict_json_with_the_call_count() {
        let s = Sample { rss_bytes: Some(1800 * MB), peak_bytes: Some(2100 * MB) };
        let v: serde_json::Value = serde_json::from_str(&sample_line(s, 7, 1)).unwrap();
        assert_eq!(v["kind"], "sample");
        assert_eq!(v["rss_mb"], 1800);
        assert_eq!(v["peak_rss_mb"], 2100);
        assert_eq!(v["calls_since_last"], 7);
        let none: serde_json::Value = serde_json::from_str(&sample_line(Sample::default(), 0, 0)).unwrap();
        assert!(none["rss_mb"].is_null(), "an unknown figure is null, not zero");
    }

    #[tokio::test]
    async fn the_sampler_writes_rows_on_a_timer_and_stops_when_told() {
        let dir = std::env::temp_dir().join(format!("procmem-sampler-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("memory-growth.log");
        let _ = std::fs::remove_file(&path);
        let (tx, rx) = tokio::sync::watch::channel(false);
        let handle = tokio::spawn(run_sampler(path.clone(), std::time::Duration::from_millis(40), rx));
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        tx.send(true).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), handle)
            .await
            .expect("the sampler stops promptly")
            .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let rows: Vec<serde_json::Value> =
            text.lines().map(|l| serde_json::from_str(l).expect("strict JSON")).collect();
        assert!(rows.len() >= 3, "a row at once and one per interval, got {}", rows.len());
        assert!(rows.iter().all(|r| r["kind"] == "sample"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Cost of the instrumentation itself, not a correctness test. Run:
    ///   cargo test --bin neurostrata-mcp overhead_probe -- --ignored --nocapture
    #[test]
    #[ignore]
    fn overhead_probe() {
        let n = 20_000u32;
        let t = std::time::Instant::now();
        for _ in 0..n {
            std::hint::black_box(sample());
        }
        let per_sample = t.elapsed() / n;
        let t = std::time::Instant::now();
        for _ in 0..n {
            let g = CallGuard::begin("neurostrata_search_memory", Some("example-project"));
            std::hint::black_box(&g);
        }
        let per_guard = t.elapsed() / n;
        println!("OVERHEAD sample() = {per_sample:?}, whole guard (begin + drop) = {per_guard:?}");
    }

    #[test]
    fn the_guard_counts_calls_in_flight() {
        let before = IN_FLIGHT.load(Ordering::SeqCst);
        let g = CallGuard::begin("t", None);
        assert_eq!(IN_FLIGHT.load(Ordering::SeqCst), before + 1);
        drop(g);
        assert_eq!(IN_FLIGHT.load(Ordering::SeqCst), before);
    }
}
