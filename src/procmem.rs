//! The daemon's own resident memory, and a log of which tool calls grew it.
//!
//! One daemon serves every console, so its memory is the sum of everything
//! every client has asked of it. `neurostrata_storage_status` reports the
//! current and peak figures; `CallGuard` wraps one MCP tool call and appends a
//! JSON line to `memory-growth.log` when the call left the process at least
//! `NEUROSTRATA_RSS_LOG_MB` (default 50) larger than it found it. After a day
//! of normal use that file names the calls that may drive growth. Background
//! growth and a neighbor that began before the window are still not
//! attributable to the call being logged.
//!
//! Calls run concurrently, so a line records `in_flight`: when it is above 1,
//! the growth may belong to a neighboring call and the line says
//! `"approximate": true`.
//!
//! Platforms: Windows, Linux and macOS report current RSS. Other Unix systems
//! report peak only (`rss_mb` is null), so the per-call growth log cannot fire
//! there; `status_json` exposes `rss_available` so callers can tell.
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
/// Monotonic count of calls that have begun, never reset. A guard whose value
/// changed before it dropped overlapped with another call even if that call
/// had already finished by the time the guard's drop sampled `IN_FLIGHT`.
static CALL_EPOCH: AtomicU64 = AtomicU64::new(0);

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

#[cfg(target_os = "macos")]
pub fn sample() -> Sample {
    // mach_task_basic_info gives both the current resident size and its
    // high-water mark; getrusage's ru_maxrss is peak-only, which made the
    // per-call growth log dead on macOS (the delta needs two current figures).
    let mut info: libc::mach_task_basic_info = unsafe { std::mem::zeroed() };
    let mut count: libc::mach_msg_type_number_t = libc::MACH_TASK_BASIC_INFO_COUNT;
    // SAFETY: `info` is a correctly sized, writable mach_task_basic_info and
    // `count` is its word count; mach_task_self() is a pseudo-port.
    let kr = unsafe {
        libc::task_info(
            libc::mach_task_self(),
            libc::MACH_TASK_BASIC_INFO,
            &mut info as *mut _ as libc::task_info_t,
            &mut count,
        )
    };
    if kr != 0 {
        return Sample::default();
    }
    Sample {
        rss_bytes: Some(info.resident_size),
        peak_bytes: Some(info.resident_size_max),
    }
}

#[cfg(all(unix, not(target_os = "linux"), not(target_os = "macos")))]
pub fn sample() -> Sample {
    // No current figure without platform-specific calls; the peak is what
    // getrusage offers. ru_maxrss is kilobytes on these BSDs.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: `usage` is a valid, writable rusage.
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) } != 0 {
        return Sample::default();
    }
    Sample {
        rss_bytes: None,
        peak_bytes: Some(usage.ru_maxrss as u64 * 1024),
    }
}

#[cfg(not(any(windows, unix)))]
pub fn sample() -> Sample {
    Sample::default()
}

fn mb(bytes: Option<u64>) -> serde_json::Value {
    bytes.map_or(serde_json::Value::Null, |b| serde_json::json!(b / MB))
}

/// The figures `neurostrata_storage_status` reports. Null where the platform
/// has none, never a made-up zero.
pub fn status_json() -> serde_json::Value {
    let s = sample();
    serde_json::json!({
        "rss_mb": mb(s.rss_bytes),
        "peak_rss_mb": mb(s.peak_bytes),
        "rss_available": s.rss_bytes.is_some(),
        "growth_log": log_path().to_string_lossy(),
        "growth_log_threshold_mb": threshold_mb(),
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
/// figures never count as growth. A threshold of 0 disables the per-call log
/// (the sampler still runs); an extreme value saturates instead of wrapping to
/// a tiny threshold and logging everything.
fn grew(before: Option<u64>, after: Option<u64>, threshold_mb: u64) -> bool {
    if threshold_mb == 0 {
        return false;
    }
    match (before, after) {
        (Some(b), Some(a)) => {
            a.saturating_sub(b) >= threshold_mb.saturating_mul(MB)
        }
        _ => false,
    }
}

fn overlapped_since(epoch_at_begin: u64, current_epoch: u64) -> bool {
    current_epoch > epoch_at_begin
}

fn line(
    tool: &str,
    namespace: Option<&str>,
    before: Sample,
    after: Sample,
    in_flight: usize,
    overlapped: bool,
) -> String {
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
        "approximate": in_flight > 1 || overlapped,
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
        if let Err(e) = append(&path, &sample_line(sample(), calls, IN_FLIGHT.load(Ordering::SeqCst))) {
            tracing::warn!("memory growth log write failed for {}: {e}", path.display());
        }
        tokio::select! {
            _ = tokio::time::sleep(every) => {}
            _ = stop.changed() => break,
        }
    }
}

static LOG_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Append one line to the growth log.
///
/// Errors are returned, not swallowed: the caller warns. A log the process
/// cannot write to is a missing diagnostic, and silently dropping it hides the
/// very growth the file exists to show.
fn append(path: &std::path::Path, line: &str) -> std::io::Result<()> {
    // Rotation and append are one critical section: concurrent calls must not
    // rename the file one is writing to or interleave a partial line.
    let _guard = LOG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if std::fs::metadata(path).map(|m| m.len() > MAX_LOG_BYTES).unwrap_or(false) {
        let mut rolled = path.as_os_str().to_owned();
        rolled.push(".1");
        if let Err(e) = std::fs::rename(path, PathBuf::from(rolled)) {
            // Losing the rotation is not losing the line: try the append.
            tracing::warn!("could not rotate {}: {e}", path.display());
        }
    }
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(f, "{line}")
}

/// Wraps one tool call. Created before the handler runs; on drop it samples
/// again and logs the call when the process grew past the threshold.
pub struct CallGuard {
    tool: String,
    namespace: Option<String>,
    before: Sample,
    epoch_at_begin: u64,
}

impl CallGuard {
    pub fn begin(tool: &str, namespace: Option<&str>) -> Self {
        IN_FLIGHT.fetch_add(1, Ordering::SeqCst);
        CALLS.fetch_add(1, Ordering::Relaxed);
        let epoch_at_begin = CALL_EPOCH.fetch_add(1, Ordering::SeqCst) + 1;
        Self {
            tool: tool.to_string(),
            namespace: namespace.map(str::to_string),
            // Disabled profiling must not pay for a syscall per call.
            before: if profile_enabled() { sample() } else { Sample::default() },
            epoch_at_begin,
        }
    }
}

impl Drop for CallGuard {
    fn drop(&mut self) {
        let in_flight = IN_FLIGHT.fetch_sub(1, Ordering::SeqCst);
        if !profile_enabled() {
            return;
        }
        let after = sample();
        // A call that began and finished while this one ran already left
        // IN_FLIGHT, but its allocation may be part of this delta.
        let overlapped = overlapped_since(self.epoch_at_begin, CALL_EPOCH.load(Ordering::SeqCst));
        if grew(self.before.rss_bytes, after.rss_bytes, threshold_mb()) {
            let log_path = log_path();
            if let Err(e) = append(
                &log_path,
                &line(
                    &self.tool,
                    self.namespace.as_deref(),
                    self.before,
                    after,
                    in_flight,
                    overlapped,
                ),
            ) {
                tracing::warn!(
                    "memory growth log write failed for {}: {e}",
                    log_path.display()
                );
            }
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
    fn zero_threshold_disables_and_extreme_saturates() {
        assert!(!grew(Some(0), Some(u64::MAX), 0), "0 disables the per-call log");
        assert!(
            !grew(Some(0), Some(MB), u64::MAX),
            "an extreme threshold saturates instead of wrapping"
        );
        assert!(grew(Some(0), Some(50 * MB), 50));
    }

    /// A log the process cannot write is reported, not silently dropped.
    #[test]
    fn growth_log_write_failure_is_surfaced() {
        let dir = std::env::temp_dir().join(format!(
            "procmem-fail-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        // Opening a directory as the append target fails on every platform.
        assert!(append(&dir, "{\"n\":1}").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn concurrent_appends_do_not_lose_or_interleave_lines() {
        let path = std::env::temp_dir().join(format!(
            "procmem-concurrent-{}-{}.log",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut handles = Vec::new();
        for i in 0..16 {
            let path = path.clone();
            handles.push(std::thread::spawn(move || {
                append(&path, &format!("{{\"n\":{i}}}")).expect("append");
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 16, "no line lost: {text}");
        for line in lines {
            serde_json::from_str::<serde_json::Value>(line).expect("each line is whole JSON");
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn this_platform_reports_a_resident_set() {
        let s = sample();
        #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
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
            serde_json::from_str(&line("neurostrata_search_memory", Some("example-project"), before, after, 2, false)).unwrap();
        assert_eq!(v["tool"], "neurostrata_search_memory");
        assert_eq!(v["namespace"], "example-project");
        assert_eq!(v["delta_mb"], 300);
        assert_eq!(v["in_flight"], 2);
    }

    #[test]
    fn a_call_that_finished_during_another_is_still_overlap() {
        assert!(!overlapped_since(7, 7), "no other call began");
        assert!(overlapped_since(7, 8), "another call began, even if it finished");
    }

    #[test]
    fn overlapping_calls_are_marked_approximate() {
        let s = Sample { rss_bytes: Some(MB), peak_bytes: Some(MB) };
        let solo: serde_json::Value = serde_json::from_str(&line("t", None, s, s, 1, false)).unwrap();
        let busy: serde_json::Value = serde_json::from_str(&line("t", None, s, s, 2, false)).unwrap();
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
            serde_json::from_str(&line("t", None, Sample::default(), Sample::default(), 1, false)).unwrap();
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
        append(&path, "{\"fresh\":true}").expect("append");
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
        assert!(p.get("rss_available").is_some(), "callers can tell whether rss is live");
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
