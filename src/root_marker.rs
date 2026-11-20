//! The project root marker: `<root>/.NeuroStrata/config.json`.
//!
//! A folder is a project root when it holds this file with a valid
//! `namespace`. The file is strict JSON; `src/schemas/root-config.schema.json`
//! documents the same rules for users, and [`read_marker`] enforces them in
//! code (the schema is not loaded at runtime).
//!
//! Writers publish a new file with a no-replace hard link so two racing
//! writers cannot both win, and rewrite an existing one with temp + rename so
//! a reader never sees half a file. Both refuse a `.NeuroStrata` that is not a
//! real directory inside the target folder. Whether a folder may be a root at
//! all (home, a drive root, a parent of checkouts) is [`refuse_root_target`],
//! which callers check before writing.

// Later tasks in the root-marker stack wire these into the writers and the
// resolver; until then nothing outside this file calls them.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// The marker file, relative to the root it marks.
pub const ROOT_CONFIG_REL: &str = ".NeuroStrata/config.json";

/// The only `version` this build understands.
const CURRENT_VERSION: u32 = 1;

/// How deep [`refuse_root_target`] looks for a `.git` below a non-git folder.
/// Checkouts are usually one to three levels under a workspace folder; the
/// bound keeps the check fast on a large tree.
const NESTED_GIT_MAX_DEPTH: usize = 4;

/// Folders the nested-checkout walk never enters: dependency and build
/// output trees are large and do not hold the user's checkouts.
const NESTED_GIT_SKIP: &[&str] = &["node_modules", "target", ".git"];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RootConfig {
    pub version: u32,
    pub namespace: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
    /// Fields this build does not know; kept so a rewrite does not drop them.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl RootConfig {
    pub fn new(namespace: impl Into<String>) -> Self {
        RootConfig {
            version: CURRENT_VERSION,
            namespace: namespace.into(),
            source_id: None,
            extra: serde_json::Map::new(),
        }
    }

    /// Why this config may not be written or trusted, or `None` when valid.
    fn invalid_reason(&self) -> Option<String> {
        if self.version != CURRENT_VERSION {
            return Some(format!("unknown version {}", self.version));
        }
        if !is_valid_namespace(&self.namespace) {
            return Some(format!("invalid namespace {:?}", self.namespace));
        }
        None
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum MarkerRead {
    Absent,
    Valid(RootConfig),
    Unreadable { path: PathBuf, reason: String },
}

#[derive(Debug, Clone, PartialEq)]
pub enum WriteOutcome {
    Written,
    /// Another writer published the file first; this is what it holds now.
    LostRace(MarkerRead),
}

#[derive(Debug)]
pub enum MarkerError {
    Unsafe(&'static str),
    Io(std::io::Error),
}

impl std::fmt::Display for MarkerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MarkerError::Unsafe(reason) => write!(f, "refused to write the root marker: {reason}"),
            MarkerError::Io(err) => write!(f, "root marker I/O error: {err}"),
        }
    }
}

impl std::error::Error for MarkerError {}

impl From<std::io::Error> for MarkerError {
    fn from(err: std::io::Error) -> Self {
        MarkerError::Io(err)
    }
}

/// `[A-Za-z0-9][A-Za-z0-9_.:-]*`, the schema's `namespace` pattern. The
/// leading letter or digit keeps a namespace from reading as an option
/// (`-x`) or a path step (`.`).
pub fn is_valid_namespace(ns: &str) -> bool {
    let mut chars = ns.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '-'))
}

/// The marker in `dir`, if any. A UTF-8 BOM is accepted; bad JSON, an
/// unknown `version` or a bad `namespace` is `Unreadable`, never `Absent`.
pub fn read_marker(dir: &Path) -> MarkerRead {
    let path = dir.join(ROOT_CONFIG_REL);
    let unreadable = |reason: String| MarkerRead::Unreadable { path: path.clone(), reason };
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return MarkerRead::Absent,
        Err(err) => return unreadable(err.to_string()),
    };
    let body = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes);
    let cfg: RootConfig = match serde_json::from_slice(body) {
        Ok(cfg) => cfg,
        Err(err) => return unreadable(format!("not a valid root config: {err}")),
    };
    match cfg.invalid_reason() {
        Some(reason) => unreadable(reason),
        None => MarkerRead::Valid(cfg),
    }
}

/// Publishes a new marker in `dir`. The file is written to a temporary name
/// in `.NeuroStrata/` and hard-linked to `config.json`, which fails when the
/// name exists: a second writer gets `LostRace` with what the first wrote.
/// There is no fallback to a replacing rename, so a filesystem without hard
/// links answers `Io`.
pub fn write_new_marker(dir: &Path, cfg: &RootConfig) -> Result<WriteOutcome, MarkerError> {
    let (tmp, target) = prepare(dir, cfg)?;
    let linked = std::fs::hard_link(&tmp, &target);
    let _ = std::fs::remove_file(&tmp);
    match linked {
        Ok(()) => Ok(WriteOutcome::Written),
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            Ok(WriteOutcome::LostRace(read_marker(dir)))
        }
        Err(err) => Err(MarkerError::Io(err)),
    }
}

/// Writes the marker in `dir`, replacing any existing one with temp + rename
/// so a concurrent reader sees either the old or the new file.
pub fn replace_marker(dir: &Path, cfg: &RootConfig) -> Result<(), MarkerError> {
    let (tmp, target) = prepare(dir, cfg)?;
    if let Err(err) = std::fs::rename(&tmp, &target) {
        let _ = std::fs::remove_file(&tmp);
        return Err(MarkerError::Io(err));
    }
    Ok(())
}

/// Validates `cfg`, checks `.NeuroStrata` is safe, and writes the
/// serialized config to a fresh temporary file beside the target.
fn prepare(dir: &Path, cfg: &RootConfig) -> Result<(PathBuf, PathBuf), MarkerError> {
    if let Some(reason) = cfg.invalid_reason() {
        return Err(MarkerError::Io(std::io::Error::new(std::io::ErrorKind::InvalidInput, reason)));
    }
    if !marker_dir_is_safe(dir) {
        return Err(MarkerError::Unsafe(".NeuroStrata is not a real folder inside the target"));
    }
    let mut body = serde_json::to_vec_pretty(cfg)
        .map_err(|err| MarkerError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, err)))?;
    body.push(b'\n');
    let target = dir.join(ROOT_CONFIG_REL);
    let tmp = temp_name(&target);
    let written = (|| {
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        file.write_all(&body)?;
        file.sync_all()
    })();
    if let Err(err) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(MarkerError::Io(err));
    }
    Ok((tmp, target))
}

fn temp_name(target: &Path) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    target.with_file_name(format!("config.json.tmp-{}-{nanos}-{n}", std::process::id()))
}

/// True when `dir/.NeuroStrata` is a real directory inside `dir` (not a
/// symlink or junction pointing elsewhere), by the check upstream uses for
/// its marker. A missing `.NeuroStrata` is created as a real directory first.
pub fn marker_dir_is_safe(dir: &Path) -> bool {
    let Ok(dir) = dunce::canonicalize(dir) else {
        return false;
    };
    let marker = dir.join(".NeuroStrata");
    if let Err(err) = std::fs::symlink_metadata(&marker) {
        if err.kind() != std::io::ErrorKind::NotFound {
            return false;
        }
        if let Err(err) = std::fs::create_dir(&marker) {
            // A racing writer may have created it; the check below decides.
            if err.kind() != std::io::ErrorKind::AlreadyExists {
                return false;
            }
        }
    }
    crate::source_context::is_valid_marker_directory(&marker)
}

/// Why `dir` may not hold a root marker, or `None` when it may:
/// - `"home folder"`: a marker at `~` would capture every project below it,
///   with or without a `.git` there;
/// - `"filesystem root"`: `/` or a drive root;
/// - `"folder contains a git checkout"`: `dir` is outside git but has a
///   `.git` below it (looked for down to [`NESTED_GIT_MAX_DEPTH`] levels,
///   skipping `node_modules`, `target` and `.git`), so it is a workspace
///   folder above projects, not a project.
pub fn refuse_root_target(dir: &Path) -> Option<&'static str> {
    refuse_root_target_with(dir, dirs::home_dir().as_deref())
}

pub(crate) fn refuse_root_target_with(dir: &Path, home: Option<&Path>) -> Option<&'static str> {
    let dir = dunce::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    if dir.parent().is_none() {
        return Some("filesystem root");
    }
    if crate::task::is_home_folder(&dir, home) {
        return Some("home folder");
    }
    if crate::paths::git_root_of_dir(&dir.to_string_lossy()).is_none() && has_git_below(&dir, 1) {
        return Some("folder contains a git checkout");
    }
    None
}

/// Whether some directory below `dir` (at `depth` and deeper, down to
/// [`NESTED_GIT_MAX_DEPTH`]) holds a `.git`. Links are not followed.
fn has_git_below(dir: &Path, depth: usize) -> bool {
    if depth > NESTED_GIT_MAX_DEPTH {
        return false;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let is_real_dir = entry.file_type().is_ok_and(|t| t.is_dir() && !t.is_symlink());
        if !is_real_dir {
            return false;
        }
        let name = entry.file_name();
        if NESTED_GIT_SKIP.iter().any(|skip| name == *skip) {
            return false;
        }
        let path = entry.path();
        path.join(".git").exists() || has_git_below(&path, depth + 1)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    fn write_raw(dir: &Path, body: &[u8]) {
        std::fs::create_dir_all(dir.join(".NeuroStrata")).unwrap();
        std::fs::write(dir.join(ROOT_CONFIG_REL), body).unwrap();
    }

    #[test]
    fn marker_round_trips_and_keeps_unknown_fields() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(read_marker(tmp.path()), MarkerRead::Absent);
        write_raw(
            tmp.path(),
            br#"{"version":1,"namespace":"example-project","source_id":"example-project","future":{"a":1}}"#,
        );
        let MarkerRead::Valid(mut cfg) = read_marker(tmp.path()) else {
            panic!("expected a valid marker");
        };
        assert_eq!(cfg.namespace, "example-project");
        assert_eq!(cfg.source_id.as_deref(), Some("example-project"));
        assert_eq!(cfg.extra.get("future"), Some(&serde_json::json!({"a": 1})));

        cfg.namespace = "example-project-2".into();
        replace_marker(tmp.path(), &cfg).unwrap();
        let raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(tmp.path().join(ROOT_CONFIG_REL)).unwrap()).unwrap();
        assert_eq!(raw["future"], serde_json::json!({"a": 1}));
        assert_eq!(raw["namespace"], "example-project-2");
        assert_eq!(read_marker(tmp.path()), MarkerRead::Valid(cfg));

        // No source_id: the field is left out, not written as null.
        let plain = RootConfig::new("example-project");
        replace_marker(tmp.path(), &plain).unwrap();
        let raw = std::fs::read_to_string(tmp.path().join(ROOT_CONFIG_REL)).unwrap();
        assert!(!raw.contains("source_id"), "{raw}");
        assert_eq!(read_marker(tmp.path()), MarkerRead::Valid(plain));
    }

    #[test]
    fn marker_with_bom_and_crlf_reads() {
        let tmp = tempfile::tempdir().unwrap();
        write_raw(
            tmp.path(),
            b"\xEF\xBB\xBF{\r\n  \"version\": 1,\r\n  \"namespace\": \"example-project\"\r\n}\r\n",
        );
        assert_eq!(read_marker(tmp.path()), MarkerRead::Valid(RootConfig::new("example-project")));
    }

    #[test]
    fn unknown_version_is_unreadable() {
        let tmp = tempfile::tempdir().unwrap();
        for body in [
            &br#"{"version":2,"namespace":"example-project"}"#[..],
            br#"{"namespace":"example-project"}"#,
            br#"{"version":1}"#,
            b"not json",
        ] {
            write_raw(tmp.path(), body);
            match read_marker(tmp.path()) {
                MarkerRead::Unreadable { path, .. } => assert_eq!(path, tmp.path().join(ROOT_CONFIG_REL)),
                other => panic!("{:?} read as {other:?}", String::from_utf8_lossy(body)),
            }
        }
        // A writer refuses a config it could not read back.
        let mut cfg = RootConfig::new("example-project");
        cfg.version = 2;
        assert!(matches!(replace_marker(tmp.path(), &cfg), Err(MarkerError::Io(_))));
    }

    #[test]
    fn namespace_charset() {
        for ok in ["team:proj", "example-project", "a", "A1_b.c:d-e"] {
            assert!(is_valid_namespace(ok), "{ok}");
        }
        for bad in ["-x", "", ".x", "_x", "a b", "a/b", "a$b", "é"] {
            assert!(!is_valid_namespace(bad), "{bad}");
        }
        let tmp = tempfile::tempdir().unwrap();
        write_raw(tmp.path(), br#"{"version":1,"namespace":"team:proj"}"#);
        assert!(matches!(read_marker(tmp.path()), MarkerRead::Valid(_)));
        write_raw(tmp.path(), br#"{"version":1,"namespace":"-x"}"#);
        assert!(matches!(read_marker(tmp.path()), MarkerRead::Unreadable { .. }));
    }

    #[test]
    fn second_writer_loses_race() {
        let tmp = tempfile::tempdir().unwrap();
        let first = RootConfig::new("example-project");
        let second = RootConfig::new("other-project");
        assert_eq!(write_new_marker(tmp.path(), &first).unwrap(), WriteOutcome::Written);
        assert_eq!(
            write_new_marker(tmp.path(), &second).unwrap(),
            WriteOutcome::LostRace(MarkerRead::Valid(first))
        );
        // No temporary files are left behind.
        let left: Vec<_> = std::fs::read_dir(tmp.path().join(".NeuroStrata"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left, vec!["config.json".to_string()]);
    }

    #[test]
    fn replace_is_atomic_for_readers() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        replace_marker(&dir, &RootConfig::new("example-a")).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let reader = {
            let (dir, stop) = (dir.clone(), stop.clone());
            std::thread::spawn(move || {
                let mut bad = Vec::new();
                let mut reads = 0u32;
                while !stop.load(Ordering::Relaxed) {
                    reads += 1;
                    let got = read_marker(&dir);
                    if !matches!(got, MarkerRead::Valid(_)) {
                        bad.push(got);
                    }
                }
                (reads, bad)
            })
        };
        for i in 0..200 {
            let ns = if i % 2 == 0 { "example-b" } else { "example-a" };
            replace_marker(&dir, &RootConfig::new(ns)).unwrap();
        }
        stop.store(true, Ordering::Relaxed);
        let (reads, bad) = reader.join().unwrap();
        assert!(reads > 0);
        assert!(bad.is_empty(), "{} of {reads} reads failed, first: {:?}", bad.len(), bad[0]);
    }

    /// Links `link` to `target` as a directory symlink, or on Windows as a
    /// junction when symlinks need a privilege this session lacks.
    fn link_dir(target: &Path, link: &Path) -> bool {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, link).is_ok()
        }
        #[cfg(windows)]
        {
            std::os::windows::fs::symlink_dir(target, link).is_ok()
                || std::process::Command::new("cmd")
                    .args(["/C", "mklink", "/J"])
                    .arg(link)
                    .arg(target)
                    .output()
                    .is_ok_and(|o| o.status.success())
        }
    }

    #[test]
    fn symlinked_neurostrata_is_refused() {
        // A missing folder is created as a real directory.
        let fresh = tempfile::tempdir().unwrap();
        assert!(marker_dir_is_safe(fresh.path()));
        assert!(std::fs::symlink_metadata(fresh.path().join(".NeuroStrata")).unwrap().is_dir());

        // A file named .NeuroStrata is not a folder.
        let file = tempfile::tempdir().unwrap();
        std::fs::write(file.path().join(".NeuroStrata"), b"").unwrap();
        assert!(!marker_dir_is_safe(file.path()));
        assert!(matches!(
            write_new_marker(file.path(), &RootConfig::new("example-project")),
            Err(MarkerError::Unsafe(_))
        ));

        // A link pointing elsewhere is refused and nothing is written there.
        let elsewhere = tempfile::tempdir().unwrap();
        let linked = tempfile::tempdir().unwrap();
        assert!(link_dir(elsewhere.path(), &linked.path().join(".NeuroStrata")), "could not create a link");
        assert!(!marker_dir_is_safe(linked.path()));
        assert!(matches!(
            write_new_marker(linked.path(), &RootConfig::new("example-project")),
            Err(MarkerError::Unsafe(_))
        ));
        assert!(matches!(
            replace_marker(linked.path(), &RootConfig::new("example-project")),
            Err(MarkerError::Unsafe(_))
        ));
        assert!(!elsewhere.path().join("config.json").exists());
    }

    #[test]
    fn home_and_root_are_refused() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(refuse_root_target_with(home.path(), Some(home.path())), Some("home folder"));
        std::fs::create_dir(home.path().join(".git")).unwrap();
        assert_eq!(refuse_root_target_with(home.path(), Some(home.path())), Some("home folder"));

        let project = home.path().join("example-project");
        std::fs::create_dir_all(project.join(".git")).unwrap();
        assert_eq!(refuse_root_target_with(&project, Some(home.path())), None);

        let fs_root = dunce::canonicalize(home.path()).unwrap().ancestors().last().unwrap().to_path_buf();
        assert_eq!(refuse_root_target_with(&fs_root, None), Some("filesystem root"));
    }

    #[test]
    fn non_git_parent_of_checkout_is_refused() {
        let ws = tempfile::tempdir().unwrap();
        let ws = ws.path();
        let plain = ws.join("notes");
        std::fs::create_dir_all(plain.join("sub")).unwrap();
        assert_eq!(refuse_root_target_with(&plain, None), None);

        let repo = ws.join("a").join("b").join("example-project");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        assert_eq!(refuse_root_target_with(ws, None), Some("folder contains a git checkout"));
        assert_eq!(refuse_root_target_with(&ws.join("a"), None), Some("folder contains a git checkout"));
        // The checkout and folders inside it may be roots.
        assert_eq!(refuse_root_target_with(&repo, None), None);
        std::fs::create_dir_all(repo.join("sub")).unwrap();
        assert_eq!(refuse_root_target_with(&repo.join("sub"), None), None);

        // Beyond the depth bound, or under a skipped folder, is not looked at.
        let deep = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(deep.path().join("1/2/3/4/5/.git")).unwrap();
        assert_eq!(refuse_root_target_with(deep.path(), None), None);
        std::fs::create_dir_all(deep.path().join("1/2/3/4/.git")).unwrap();
        assert_eq!(refuse_root_target_with(deep.path(), None), Some("folder contains a git checkout"));
        let skipped = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(skipped.path().join("node_modules/pkg/.git")).unwrap();
        std::fs::create_dir_all(skipped.path().join("target/x/.git")).unwrap();
        assert_eq!(refuse_root_target_with(skipped.path(), None), None);
    }
}
