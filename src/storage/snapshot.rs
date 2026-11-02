use std::fmt;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const SNAPSHOT_FORMAT: &str = "neurostrata-snapshot";
pub const SNAPSHOT_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StorageHygieneConfig {
    pub checkpoint_interval_secs: u64,
    pub wal_warn_bytes: u64,
    pub snapshot_retention_count: usize,
}

impl Default for StorageHygieneConfig {
    fn default() -> Self {
        Self {
            checkpoint_interval_secs: 2,
            wal_warn_bytes: 64 * 1024 * 1024,
            snapshot_retention_count: 5,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageStatus {
    pub store_path: String,
    pub store_bytes: u64,
    pub wal_bytes: u64,
    pub wal_warn_threshold_exceeded: bool,
    pub snapshot_count: usize,
    pub retained_limit: usize,
    // Spec §5.5 (2026-10-28 storage-hygiene design).
    pub dirty: bool,
    pub checkpoint_interval_secs: u64,
    pub last_checkpoint_at: Option<String>,
    pub last_checkpoint_duration_ms: Option<u64>,
    pub checkpoint_failures: u32,
    pub snapshot_export_dir: String,
}

/// Duration of the most recent checkpoint, in milliseconds. Cross-task record
/// because checkpoints are driven by whichever task lands the write.
static LAST_CHECKPOINT_DURATION_MS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Unix-seconds timestamp of the most recent checkpoint recorded in this
/// process (0 = none recorded yet). The directory mtime is only a proxy;
/// this records the real event the moment it happens.
static LAST_CHECKPOINT_AT_SECS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Count of failed checkpoints since process start (monotonic, never reset).
static CHECKPOINT_FAILURES: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(0);

/// Record a finished checkpoint. Called from the store's checkpoint path so
/// `compute_storage_status` reports real hygiene data instead of constants.
pub fn record_checkpoint_outcome(duration_ms: u64, ok: bool) {
    use std::sync::atomic::Ordering;
    LAST_CHECKPOINT_DURATION_MS.store(duration_ms, Ordering::Relaxed);
    LAST_CHECKPOINT_AT_SECS.store(
        chrono::Utc::now().timestamp().max(0) as u64,
        Ordering::Relaxed,
    );
    if !ok {
        CHECKPOINT_FAILURES.fetch_add(1, Ordering::Relaxed);
    }
}

pub fn compute_storage_status(
    store_path: &Path,
    config: &StorageHygieneConfig,
) -> StorageStatus {
    let store_bytes = fs::metadata(store_path).map(|m| m.len()).unwrap_or(0);
    let mut wal_os = store_path.as_os_str().to_os_string();
    wal_os.push(".wal");
    let wal_path = PathBuf::from(wal_os);
    let wal_bytes = fs::metadata(&wal_path).map(|m| m.len()).unwrap_or(0);
    let snapshot_dir = store_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    let snapshot_read = snapshot_dir.read_dir();
    let snapshot_count = match snapshot_read {
        Ok(entries) => entries
            .filter_map(|res| match res {
                Ok(e) => Some(e),
                Err(e) => {
                    tracing::warn!("snapshot directory entry read failed: {e}");
                    None
                }
            })
            .filter(|e| {
                e.path().is_dir()
                    && e.file_name()
                        .to_str()
                        .map(|s| s.starts_with("snapshot-"))
                        .unwrap_or(false)
            })
            .count(),
        Err(e) => {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!("snapshot read_dir failed: {e}");
            }
            0
        }
    };
    // Prefer the real checkpoint timestamp recorded in this process; the
    // store directory's mtime is only a proxy and falls back when no
    // checkpoint happened yet in this process.
    let last_checkpoint_at = {
        let recorded_ts = LAST_CHECKPOINT_AT_SECS.load(std::sync::atomic::Ordering::Relaxed);
        if recorded_ts > 0 {
            chrono::DateTime::from_timestamp(recorded_ts as i64, 0)
                .map(|dt| dt.to_rfc3339())
                .or_else(|| {
                    fs::metadata(&snapshot_dir)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|mtime| mtime.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| {
                            let dt = chrono::DateTime::<chrono::Utc>::from(
                                std::time::UNIX_EPOCH + d,
                            );
                            dt.to_rfc3339()
                        })
                })
        } else {
            fs::metadata(&snapshot_dir)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|mtime| mtime.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| {
                    let dt = chrono::DateTime::<chrono::Utc>::from(
                        std::time::UNIX_EPOCH + d,
                    );
                    dt.to_rfc3339()
                })
        }
    };
    StorageStatus {
        store_path: store_path.display().to_string(),
        store_bytes,
        wal_bytes,
        wal_warn_threshold_exceeded: wal_bytes > config.wal_warn_bytes,
        snapshot_count,
        retained_limit: config.snapshot_retention_count.max(1),
        dirty: wal_bytes > 0,
        checkpoint_interval_secs: config.checkpoint_interval_secs,
        last_checkpoint_at,
        // Only a checkpoint that actually happened in this process claims a
        // duration: before the first checkpoint the atomic reads 0, and
        // reporting `Some(0)` would be a fabricated metric. Mirrors the
        // `last_checkpoint_at` guard above, which falls back to the
        // directory mtime for the timestamp.
        last_checkpoint_duration_ms: if LAST_CHECKPOINT_AT_SECS.load(std::sync::atomic::Ordering::Relaxed) > 0
        {
            Some(LAST_CHECKPOINT_DURATION_MS.load(std::sync::atomic::Ordering::Relaxed))
        } else {
            None
        },
        checkpoint_failures: CHECKPOINT_FAILURES.load(std::sync::atomic::Ordering::Relaxed),
        snapshot_export_dir: snapshot_dir.display().to_string(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotChecksums {
    pub ladybug_store_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wal_sha256: Option<String>,
    pub manifest_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotVerifyReport {
    pub valid: bool,
    pub dir: PathBuf,
    pub checksums: Option<SnapshotChecksums>,
    pub error: Option<String>,
}

#[derive(Debug)]
pub enum SnapshotError {
    Io(std::io::Error),
    Json(serde_json::Error),
    MissingFile(String),
    InvalidManifest(String),
    ChecksumMismatch {
        file: String,
        expected: String,
        actual: String,
    },
}

impl fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SnapshotError::Io(e) => write!(f, "io error: {}", e),
            SnapshotError::Json(e) => write!(f, "json error: {}", e),
            SnapshotError::MissingFile(s) => write!(f, "missing file: {}", s),
            SnapshotError::InvalidManifest(s) => write!(f, "invalid manifest: {}", s),
            SnapshotError::ChecksumMismatch {
                file,
                expected,
                actual,
            } => write!(
                f,
                "checksum mismatch for {}: expected {}, got {}",
                file, expected, actual
            ),
        }
    }
}

impl std::error::Error for SnapshotError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SnapshotError::Io(e) => Some(e),
            SnapshotError::Json(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for SnapshotError {
    fn from(e: std::io::Error) -> Self {
        SnapshotError::Io(e)
    }
}

impl From<serde_json::Error> for SnapshotError {
    fn from(e: serde_json::Error) -> Self {
        SnapshotError::Json(e)
    }
}

pub fn sha256_file(path: &Path) -> Result<String, SnapshotError> {
    let mut hasher = Sha256::new();
    if path.is_dir() {
        let mut entries: Vec<fs::DirEntry> = fs::read_dir(path)?
            .filter_map(|res| match res {
                Ok(e) => Some(e),
                Err(e) => {
                    tracing::warn!("snapshot directory entry read failed: {e}");
                    None
                }
            })
            .filter(|e| {
                e.path().is_file() || e.file_type().map(|ft| ft.is_file()).unwrap_or(false)
            })
            .collect();
        entries.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
        for entry in entries {
            let p = entry.path();
            let mut file = File::open(&p)?;
            let mut buf = [0u8; 65536];
            loop {
                let n = file.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                hasher.update(&buf[..n]);
            }
        }
    } else {
        let mut file = File::open(path)?;
        let mut buf = [0u8; 65536];
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
    }
    let digest = hasher.finalize();
    Ok(digest.iter().map(|b| format!("{:02x}", b)).collect::<String>())
}

pub fn write_checksums(dir: &Path) -> Result<SnapshotChecksums, SnapshotError> {
    let ladybug = dir.join("ladybug.store");
    let wal = dir.join("ladybug.store.wal");
    let manifest = dir.join("manifest.json");

    if !ladybug.exists() && !ladybug.is_dir() {
        return Err(SnapshotError::MissingFile(
            "ladybug.store".into(),
        ));
    }
    if !manifest.exists() {
        return Err(SnapshotError::MissingFile(
            "manifest.json".into(),
        ));
    }

    let ladybug_sha = sha256_file(&ladybug)?;
    let wal_sha = if wal.exists() {
        Some(sha256_file(&wal)?)
    } else {
        None
    };
    let manifest_sha = sha256_file(&manifest)?;

    let checksums = SnapshotChecksums {
        ladybug_store_sha256: ladybug_sha,
        wal_sha256: wal_sha,
        manifest_sha256: manifest_sha,
    };

    let checksums_path = dir.join("checksums.json");
    let mut file = File::create(&checksums_path)?;
    let json = serde_json::to_string_pretty(&checksums)?;
    file.write_all(json.as_bytes())?;
    file.write_all(b"\n")?;

    // zstd-compressed manifest copy for space-efficient transport.
    if manifest.exists() {
        let raw = fs::read(&manifest)?;
        let compressed = crate::compression::zstd::compress_snapshot(&raw)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        let mut zst = File::create(dir.join("manifest.json.zst"))?;
        zst.write_all(&compressed)?;
    }

    Ok(checksums)
}

pub fn verify_snapshot(dir: &Path) -> Result<SnapshotVerifyReport, SnapshotError> {
    let manifest_path = dir.join("manifest.json");
    if !manifest_path.exists() {
        return Err(SnapshotError::MissingFile(
            "manifest.json".into(),
        ));
    }
    let manifest_str = fs::read_to_string(&manifest_path)?;
    let manifest: serde_json::Value = serde_json::from_str(&manifest_str)?;
    let format = manifest.get("format").and_then(|v| v.as_str()).unwrap_or("");
    let version = manifest.get("version").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    if format != SNAPSHOT_FORMAT || version != SNAPSHOT_VERSION {
        return Err(SnapshotError::InvalidManifest(format!(
            "expected format={} version={}, got format={:?} version={:?}",
            SNAPSHOT_FORMAT, SNAPSHOT_VERSION, format, version
        )));
    }

    let checksums_path = dir.join("checksums.json");
    if !checksums_path.exists() {
        return Err(SnapshotError::MissingFile(
            "checksums.json".into(),
        ));
    }
    let checksums_str = fs::read_to_string(&checksums_path)?;
    let checksums: SnapshotChecksums = serde_json::from_str(&checksums_str)?;

    let ladybug = dir.join("ladybug.store");
    let manifest_path = dir.join("manifest.json");
    let wal = dir.join("ladybug.store.wal");

    if !ladybug.exists() && !ladybug.is_dir() {
        return Err(SnapshotError::MissingFile(
            "ladybug.store".into(),
        ));
    }

    let computed_ladybug = sha256_file(&ladybug)?;
    if computed_ladybug != checksums.ladybug_store_sha256 {
        return Err(SnapshotError::ChecksumMismatch {
            file: "ladybug.store".into(),
            expected: checksums.ladybug_store_sha256.clone(),
            actual: computed_ladybug,
        });
    }

    let computed_manifest = sha256_file(&manifest_path)?;
    if computed_manifest != checksums.manifest_sha256 {
        return Err(SnapshotError::ChecksumMismatch {
            file: "manifest.json".into(),
            expected: checksums.manifest_sha256.clone(),
            actual: computed_manifest,
        });
    }

    let wal_mentioned = checksums.wal_sha256.is_some() || wal.exists();
    if wal_mentioned {
        if !wal.exists() {
            return Err(SnapshotError::ChecksumMismatch {
                file: "ladybug.store.wal".into(),
                expected: checksums.wal_sha256.clone().unwrap_or_default(),
                actual: "<missing>".into(),
            });
        }
        let computed_wal = sha256_file(&wal)?;
        if Some(&computed_wal) != checksums.wal_sha256.as_ref() {
            return Err(SnapshotError::ChecksumMismatch {
                file: "ladybug.store.wal".into(),
                expected: checksums.wal_sha256.clone().unwrap_or_default(),
                actual: computed_wal,
            });
        }
    }

    // If a zstd-compressed manifest copy is present, verify it decompresses
    // within the bounded snapshot size cap AND that its content matches the
    // plain `manifest.json` bytes: a compressed copy drifting from the manifest
    // is an inconsistent snapshot, not a valid one.
    let zst_path = dir.join("manifest.json.zst");
    if zst_path.exists() {
        let raw = fs::read(&zst_path)?;
        let decompressed = crate::compression::zstd::decompress_snapshot(
            &raw,
            crate::compression::zstd::DEFAULT_SNAPSHOT_MAX_DECOMPRESSED_BYTES,
        )
        .map_err(|e| {
            SnapshotError::InvalidManifest(format!("manifest.json.zst: {}", e))
        })?;
        let expected_raw = fs::read(&manifest_path)?;
        if decompressed != expected_raw {
            return Err(SnapshotError::InvalidManifest(
                "manifest.json.zst content mismatch with manifest.json".to_string(),
            ));
        }
    }

    Ok(SnapshotVerifyReport {
        valid: true,
        dir: dir.to_path_buf(),
        checksums: Some(checksums),
        error: None,
    })
}

pub fn select_expired_snapshots(root: &Path, keep: usize) -> Result<Vec<PathBuf>, SnapshotError> {
    let keep = keep.max(1);
    let mut valid: Vec<(PathBuf, String, String)> = Vec::new();

    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name().into_string().unwrap_or_default();
        if !name.starts_with("snapshot-") {
            continue;
        }
        if path == root {
            continue;
        }
        let manifest_path = path.join("manifest.json");
        if !manifest_path.exists() {
            continue;
        }
        let manifest_str = match fs::read_to_string(&manifest_path) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(
                    "unreadable snapshot manifest at {}: {e}",
                    manifest_path.display()
                );
                continue;
            }
        };
        let manifest: serde_json::Value = match serde_json::from_str(&manifest_str) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    "invalid JSON in snapshot manifest at {}: {e}",
                    manifest_path.display()
                );
                continue;
            }
        };
        let format = manifest.get("format").and_then(|v| v.as_str()).unwrap_or("");
        let version = manifest.get("version").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        if format != SNAPSHOT_FORMAT || version != SNAPSHOT_VERSION {
            continue;
        }
        let created_at = manifest.get("created_at").and_then(|v| v.as_str()).unwrap_or("");
        if created_at.is_empty() {
            continue;
        }
        valid.push((path.clone(), created_at.to_string(), name));
    }

    valid.sort_by(|a, b| {
        b.1.cmp(&a.1).then_with(|| b.2.cmp(&a.2))
    });

    let expired = if valid.len() > keep {
        valid[keep..].iter().map(|(p, _, _)| p.clone()).collect()
    } else {
        Vec::new()
    };
    Ok(expired)
}

pub fn prune_expired_snapshots(root: &Path, keep: usize) -> Result<Vec<PathBuf>, SnapshotError> {
    let to_remove = select_expired_snapshots(root, keep)?;
    let mut removed = Vec::new();
    for path in &to_remove {
        fs::remove_dir_all(path)?;
        removed.push(path.clone());
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_dir(name: &str) -> PathBuf {
        let base = std::env::temp_dir().join("neurostrata-snapshot-tests");
        let d = base.join(name);
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn make_snapshot_dir(parent: &Path, snap_name: &str, created_at: &str) -> PathBuf {
        let d = parent.join(snap_name);
        fs::create_dir_all(&d).unwrap();
        let mut manifest = File::create(d.join("manifest.json")).unwrap();
        let content = format!(
            "{{\"format\":\"{}\",\"version\":{},\"created_at\":\"{}\"}}",
            SNAPSHOT_FORMAT, SNAPSHOT_VERSION, created_at
        );
        manifest.write_all(content.as_bytes()).unwrap();
        d
    }

    fn write_store(dir: &Path) {
        let mut f = File::create(dir.join("ladybug.store")).unwrap();
        f.write_all(b"test store content").unwrap();
    }

    #[test]
    fn roundtrip_write_and_verify() {
        let d = temp_dir("roundtrip");
        write_store(&d);
        let mut manifest = File::create(d.join("manifest.json")).unwrap();
        let content = format!(
            "{{\"format\":\"{}\",\"version\":{},\"created_at\":\"2026-01-01T00:00:00Z\"}}",
            SNAPSHOT_FORMAT, SNAPSHOT_VERSION
        );
        manifest.write_all(content.as_bytes()).unwrap();
        let checksums = write_checksums(&d).unwrap();
        assert!(!checksums.ladybug_store_sha256.is_empty());
        assert!(checksums.manifest_sha256.len() == 64);
        let report = verify_snapshot(&d).unwrap();
        assert!(report.valid);
        assert_eq!(report.dir, d);
        assert!(report.error.is_none());
    }

    #[test]
    fn tampered_file_triggers_checksum_mismatch() {
        let d = temp_dir("tampered");
        write_store(&d);
        let mut manifest = File::create(d.join("manifest.json")).unwrap();
        let content = format!(
            "{{\"format\":\"{}\",\"version\":{},\"created_at\":\"2026-01-01T00:00:00Z\"}}",
            SNAPSHOT_FORMAT, SNAPSHOT_VERSION
        );
        manifest.write_all(content.as_bytes()).unwrap();
        write_checksums(&d).unwrap();
        // Tamper store after checksums written
        let mut f = File::create(d.join("ladybug.store")).unwrap();
        f.write_all(b"tampered").unwrap();
        let result = verify_snapshot(&d);
        assert!(matches!(
            result,
            Err(SnapshotError::ChecksumMismatch { .. })
        ));
    }

    #[test]
    fn invalid_manifest_triggers_invalid_manifest() {
        let d = temp_dir("invalid_manifest");
        write_store(&d);
        let mut manifest = File::create(d.join("manifest.json")).unwrap();
        manifest.write_all(b"{\"format\":\"bad\",\"version\":99}").unwrap();
        let mut f = File::create(d.join("checksums.json")).unwrap();
        f.write_all(b"{}").unwrap();
        let result = verify_snapshot(&d);
        assert!(matches!(
            result,
            Err(SnapshotError::InvalidManifest(..))
        ));
    }

    #[test]
    fn select_expired_keeps_newest_n_and_ignores_invalid() {
        let root = temp_dir("expired_select");
        // Create 3 valid snapshots
        make_snapshot_dir(&root, "snapshot-old1", "2025-01-01T00:00:00Z");
        make_snapshot_dir(&root, "snapshot-new1", "2026-01-01T00:00:00Z");
        make_snapshot_dir(&root, "snapshot-new2", "2026-06-01T00:00:00Z");
        // Invalid directory without manifest
        fs::create_dir_all(root.join("snapshot-bad")).unwrap();
        // Directory with bad manifest
        let bad = root.join("snapshot-bad-manifest");
        fs::create_dir_all(&bad).unwrap();
        let mut mf = File::create(bad.join("manifest.json")).unwrap();
        mf.write_all(b"{\"format\":\"bad\"}").unwrap();

        let expired = select_expired_snapshots(&root, 2).unwrap();
        assert_eq!(expired.len(), 1);
        assert!(expired[0].file_name().unwrap().to_string_lossy().contains("old1"));
    }

    #[test]
    fn select_expired_clamps_keep_to_one() {
        let root = temp_dir("clamp");
        make_snapshot_dir(&root, "snapshot-a", "2026-01-01T00:00:00Z");
        let expired = select_expired_snapshots(&root, 0).unwrap();
        assert!(expired.is_empty());
    }

    #[test]
    fn prune_expired_removes_oldest() {
        let root = temp_dir("prune");
        make_snapshot_dir(&root, "snapshot-old", "2025-01-01T00:00:00Z");
        make_snapshot_dir(&root, "snapshot-new", "2026-01-01T00:00:00Z");
        let removed = prune_expired_snapshots(&root, 1).unwrap();
        assert_eq!(removed.len(), 1);
        assert!(!root.join("snapshot-old").exists());
        assert!(root.join("snapshot-new").exists());
    }
}
