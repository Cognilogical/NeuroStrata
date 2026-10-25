//! Episodic Buffer — the rolling session log documented in README
//! ("The Episodic Buffer & Operator Controls"). `neurostrata_append_log`
//! writes timestamped entries to `<project_root>/.NeuroStrata/sessions/current.md`,
//! injecting a `### 🔄 Topic Switch` marker on tagged turns, rolling the file
//! at 500KB, and pruning rolled files past the retention window.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::domain::normalize_domain_slug;
use crate::traits::{DomainMatchMode, Embedder, VectorStore};

/// Roll `current.md` once it would cross this size (README: 500KB).
const ROLLOVER_BYTES: u64 = 500 * 1024;
/// Retention when `buffer_retention_days` is unset in the config.
const DEFAULT_RETENTION_DAYS: u64 = 30;

/// Buffer-relevant slice of `~/.config/neurostrata/config.json`.
/// Read as raw JSON rather than through `config::Config` so the buffer's two
/// optional keys cannot perturb that struct's invariants and legacy handling.
pub(crate) struct BufferConfig {
    pub enabled: bool,
    pub retention_days: u64,
}

pub(crate) fn load_config() -> BufferConfig {
    let path = dirs::home_dir().map(|h| h.join(".config").join("neurostrata").join("config.json"));
    let raw = path
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str::<Value>(&s).ok());
    let value = raw.unwrap_or_else(|| json!({}));
    BufferConfig {
        enabled: value
            .get("episodic_buffer")
            .and_then(|v| v.as_bool())
            .unwrap_or(true),
        retention_days: value
            .get("buffer_retention_days")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_RETENTION_DAYS),
    }
}

/// What one append did, for the return payload and tests.
pub(crate) struct Outcome {
    pub file: String,
    pub size_bytes: u64,
    pub rolled: bool,
}

/// Secret scan over the entry before anything touches disk. Returns the
/// rejection text when the content looks like a secret.
pub(crate) fn scan_for_append(content: &str, tags: &[String]) -> Option<String> {
    crate::secrets::scan_entry_point(content, &json!({ "tags": tags }), "neurostrata_append_log")
        .map(|r| r.to_string())
}

/// The J22 refusal, in one place so the pre-creation judgment and the
/// post-creation verification in [`append_entry`] speak identically.
fn sessions_outside_root(
    sessions_path: &Path,
    resolved: &Path,
    canonical_root: &Path,
) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        format!(
            "refused: the sessions directory {} resolves to {}, outside \
             the project root {} — nothing was written",
            sessions_path.display(),
            resolved.display(),
            canonical_root.display()
        ),
    )
}

/// Where `path` would resolve, judged without creating anything: the
/// nearest existing ancestor canonicalized, the remainder rejoined as
/// plain components — the `check_caller_path` pattern (daemon.rs). A
/// remainder component with no plain file name (`..`, `.`) cannot be
/// judged this way and yields `None`.
fn judged_resolution(path: &Path) -> Option<PathBuf> {
    let mut cursor = path.to_path_buf();
    let mut remainder: Vec<std::ffi::OsString> = Vec::new();
    loop {
        if let Ok(canonical) = std::fs::canonicalize(&cursor) {
            let mut resolved = canonical;
            for name in remainder.iter().rev() {
                resolved.push(name);
            }
            return Some(resolved);
        }
        match (cursor.file_name().map(|n| n.to_os_string()), cursor.parent()) {
            (Some(name), Some(parent)) => {
                remainder.push(name);
                cursor = parent.to_path_buf();
            }
            _ => return None,
        }
    }
}

/// Append one entry (and its optional topic-switch marker) to the buffer.
/// `max_bytes` is injectable so tests do not have to write 500KB.
///
/// Path discipline (Task 2 / J22): the sessions directory is canonicalized
/// and must lie inside the canonical project root before anything opens —
/// a `.NeuroStrata` or `sessions` symlink planted in the tree cannot
/// redirect the write out of the project. Containment is judged BEFORE
/// anything is created (fix round / M-1), so the refusal never follows a
/// planted link into creating directories outside the project on its way
/// to claiming nothing was written. The session file itself is
/// opened with `O_NOFOLLOW` on Unix, and a final component that is a
/// symlink is a loud error with nothing written: the pre-open check names
/// the refusal, and the flag closes the check-to-open race.
pub(crate) fn append_entry(
    project_root: &Path,
    content: &str,
    tags: &[String],
    max_bytes: u64,
    retention_days: u64,
) -> std::io::Result<Outcome> {
    let sessions_path = project_root.join(".NeuroStrata").join("sessions");
    // Judge containment before creating anything (M-1): canonicalize the
    // nearest existing ancestor of the sessions path and judge the
    // remainder. When the root itself does not exist yet, nothing beneath
    // it exists either — no link can have been planted — and the
    // post-creation check below remains the authority.
    if let Ok(canonical_root) = std::fs::canonicalize(project_root) {
        if let Some(judged) = judged_resolution(&sessions_path) {
            if !judged.starts_with(&canonical_root) {
                return Err(sessions_outside_root(
                    &sessions_path,
                    &judged,
                    &canonical_root,
                ));
            }
        }
    }
    std::fs::create_dir_all(&sessions_path)?;
    let canonical_root = std::fs::canonicalize(project_root)?;
    let sessions = std::fs::canonicalize(&sessions_path)?;
    if !sessions.starts_with(&canonical_root) {
        return Err(sessions_outside_root(
            &sessions_path,
            &sessions,
            &canonical_root,
        ));
    }
    let current = sessions.join("current.md");
    if let Ok(link) = std::fs::symlink_metadata(&current) {
        if link.file_type().is_symlink() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!(
                    "refused: the session file {} is a symlink — the episodic \
                     buffer never writes through a link; nothing was written",
                    current.display()
                ),
            ));
        }
    }

    let stamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
    let mut entry = String::new();
    if !tags.is_empty() {
        entry.push_str(&format!("### 🔄 Topic Switch [{}]\n", tags.join(", ")));
    }
    entry.push_str(&format!("- *[{}]*: {}\n", stamp, content));

    let existing = std::fs::metadata(&current).map(|m| m.len()).unwrap_or(0);
    let mut rolled = false;
    if existing > 0 && existing + entry.len() as u64 > max_bytes {
        // Nanos in the name: two rolls inside one second must not clobber.
        let roll_name = format!(
            "session-{}.md",
            chrono::Local::now().format("%Y%m%d-%H%M%S-%f")
        );
        std::fs::rename(&current, sessions.join(&roll_name))?;
        rolled = true;
        prune_rolled(&sessions, retention_days)?;
    }

    use std::io::Write;
    let mut file = open_session_file(&current)?;
    file.write_all(entry.as_bytes())?;
    let size_bytes = std::fs::metadata(&current).map(|m| m.len()).unwrap_or(0);

    Ok(Outcome {
        file: current.display().to_string(),
        size_bytes,
        rolled,
    })
}

/// Open the session file for appending. On Unix the open carries
/// `O_NOFOLLOW`, so a symlink swapped in after `append_entry`'s pre-open
/// check still cannot be written through — the kernel refuses it (ELOOP)
/// and the refusal is translated into the same words as the check's.
#[cfg(unix)]
fn open_session_file(current: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut options = std::fs::OpenOptions::new();
    options
        .create(true)
        .append(true)
        .custom_flags(libc::O_NOFOLLOW);
    options.open(current).map_err(|e| {
        if e.raw_os_error() == Some(libc::ELOOP) {
            std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!(
                    "refused: the session file {} is a symlink — the episodic \
                     buffer never writes through a link; nothing was written",
                    current.display()
                ),
            )
        } else {
            e
        }
    })
}

/// Open the session file for appending (non-Unix: `append_entry`'s
/// pre-open symlink check is the guard; there is no `O_NOFOLLOW` to add).
#[cfg(not(unix))]
fn open_session_file(current: &Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(current)
}

/// Delete rolled `session-*.md` files older than the retention window.
fn prune_rolled(sessions: &Path, retention_days: u64) -> std::io::Result<()> {
    let cutoff = std::time::Duration::from_secs(retention_days * 24 * 60 * 60);
    let now = std::time::SystemTime::now();
    if let Ok(entries) = std::fs::read_dir(sessions) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !name.starts_with("session-") || !name.ends_with(".md") {
                continue;
            }
            let old = entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| now.duration_since(t).ok())
                .map(|age| age > cutoff)
                .unwrap_or(false);
            if old {
                std::fs::remove_file(entry.path())?;
            }
        }
    }
    Ok(())
}

pub async fn handle_append_log(
    arguments: Value,
    emb: Arc<dyn Embedder>,
    store: Arc<dyn VectorStore>,
) -> String {
    let content = match arguments.get("content").and_then(|v| v.as_str()) {
        Some(s) if !s.trim().is_empty() => s.to_string(),
        _ => {
            return "ERROR: 'content' is required and must be a non-empty string.".to_string();
        }
    };
    let project_root = match arguments.get("project_root").and_then(|v| v.as_str()) {
        Some(s) if !s.trim().is_empty() => std::path::PathBuf::from(s),
        _ => {
            return "ERROR: 'project_root' is required (the absolute path to the project root; the buffer lives at <project_root>/.NeuroStrata/sessions/)."
                .to_string();
        }
    };
    let tags: Vec<String> = arguments
        .get("tags")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|t| t.as_str())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();

    if let Some(rejection) = scan_for_append(&content, &tags) {
        return rejection;
    }

    let config = load_config();
    if !config.enabled {
        return "Episodic Buffer disabled (config: \"episodic_buffer\": false); entry not written."
            .to_string();
    }

    // Build the query and key set from normalized tags
    let slugs: Vec<String> = tags
        .iter()
        .filter_map(|t| normalize_domain_slug(t))
        .collect();

    // First, do the append
    let append_result = append_entry(
        &project_root,
        &content,
        &tags,
        ROLLOVER_BYTES,
        config.retention_days,
    );

    // If append failed, return failure response immediately (no search runs)
    let outcome = match append_result {
        Ok(outcome) => outcome,
        Err(e) => {
            tracing::warn!("Failed to append to the Episodic Buffer: {}", e);
            let resp = json!({
                "appended": false,
                "error": format!("could not append to the Episodic Buffer: {}", e),
                "file": "",
                "size_bytes": 0,
                "rolled": false,
                "tags": tags,
                "recalled": [],
                "task_intent_signals": [],
            });
            return resp.to_string();
        }
    };

    // Append succeeded - now run the recall if we have slugs
    let (recalled, recalled_from) = if slugs.is_empty() {
        (vec![], None)
    } else {
        // Resolve namespace only when we actually need to search (F5)
        let namespace_arg = arguments
            .get("namespace")
            .and_then(|v| v.as_str())
            .map(String::from);
        let namespace_candidate = if let Some(ns) = namespace_arg {
            if ns.trim().is_empty() {
                // Treat empty/whitespace-only namespace as absent (F7)
                project_root
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(String::from)
                    .unwrap_or_else(|| "default".to_string())
            } else {
                ns
            }
        } else {
            project_root
                .file_name()
                .and_then(|n| n.to_str())
                .map(String::from)
                .unwrap_or_else(|| "default".to_string())
        };
        let namespace = crate::server::resolve_namespace(&store, &namespace_candidate).await;

        // Embed the joined query once
        let query = slugs.join(" ");
        let vec = match emb.embed(&query).await {
            Ok(v) => v,
            Err(_) => {
                tracing::warn!("Failed to generate embedding for recall");
                return success_response(&outcome, &tags, vec![], None, vec![]);
            }
        };

        // Initialize namespace before searching
        if let Err(e) = store.init(&namespace).await {
            tracing::warn!("Failed to initialize namespace for recall: {}", e);
            return success_response(&outcome, &tags, vec![], None, vec![]);
        }

        // Run the recall search
        let search_outcome = match store
            .search_with_domain(
                &namespace,
                vec,
                3,              // limit = 3
                false,          // include_tasks = false
                false,          // include_archived = false
                &slugs,
            )
            .await
        {
            Ok(o) => o,
            Err(e) => {
                tracing::warn!("Recall search failed: {}", e);
                return success_response(&outcome, &tags, vec![], None, vec![]);
            }
        };

        // Domain-filter counters: this search carried a filter (slugs non-empty).
        let mode = search_outcome.mode;
        let single_domain =
            crate::registry::registry_domain_count(&store, &namespace).await <= 1;
        crate::recall_metrics::record_domain_query(
            mode == crate::traits::DomainMatchMode::Domain,
            mode == crate::traits::DomainMatchMode::Vector,
            single_domain,
        );

        // Keep only direct matches
        let direct_matches: Vec<_> = search_outcome
            .results
            .into_iter()
            .filter(|res| crate::traits::is_direct_match(&res.evidence))
            .take(3)
            .map(|res| {
                let domain = crate::domain::domain_key(&res.payload.metadata);
                let prefix = if crate::attempt::is_failed_attempt(&res.payload.metadata) {
                    crate::attempt::FAILED_ATTEMPT_PREFIX
                } else {
                    ""
                };
                let budget = 120usize.saturating_sub(prefix.chars().count());
                let snippet = format!(
                    "{}{}",
                    prefix,
                    res.payload.content.chars().take(budget).collect::<String>()
                );
                json!({
                    "id": res.id,
                    "memory_type": res.payload.memory_type,
                    "domain": domain,
                    "score": res.score,
                    "snippet": snippet,
                })
            })
            .collect();

        let recalled_ids: Vec<String> = direct_matches
            .iter()
            .filter_map(|v| v.get("id").and_then(|x| x.as_str()).map(String::from))
            .collect();
        crate::recall_metrics::record_recall(&namespace, &recalled_ids);

        let recalled_from_str = match mode {
            DomainMatchMode::Domain => Some("domain"),
            DomainMatchMode::Vector => Some("vector"),
        };

        (direct_matches, recalled_from_str)
    };

    // Build the success response
    let signals: Vec<Value> = crate::task::intent::detect_task_intents(&content)
        .into_iter()
        .map(|s| {
            json!({
                "marker": s.marker,
                "excerpt": s.excerpt,
                "action": "Call neurostrata_task_create(namespace, title) so this work does not evaporate."
            })
        })
        .collect();
    success_response(&outcome, &tags, recalled, recalled_from, signals)
}

/// Builds the append_log success response — one shape for every successful append.
///
/// `recalled` is the recall's rows, an empty array when nothing was recalled.
/// `recalled_from` is present exactly when a search ran.
fn success_response(
    outcome: &Outcome,
    tags: &[String],
    recalled: Vec<Value>,
    recalled_from: Option<&str>,
    task_intent_signals: Vec<Value>,
) -> String {
    let mut resp = json!({
        "appended": true,
        "file": outcome.file,
        "size_bytes": outcome.size_bytes,
        "rolled": outcome.rolled,
        "tags": tags,
        "recalled": recalled,
        "task_intent_signals": task_intent_signals,
    });
    if let Some(mode) = recalled_from {
        resp["recalled_from"] = json!(mode);
    }
    resp.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::guard::testkit::{MockStore, StubEmbedder};
    use crate::traits::Embedder;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "neurostrata-buffer-test-{}-{}",
            std::process::id(),
            tag
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A counting embedder to assert exactly one embed call per tagged turn
    /// and zero for untagged turns.
    struct CountingEmbedder {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl Embedder for CountingEmbedder {
        async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(vec![0.1, 0.2, 0.3, 0.4])
        }
        fn dimensions(&self) -> usize {
            4
        }
    }

    #[test]
    fn append_with_tags_injects_topic_switch_marker() {
        let root = scratch("marker");
        let outcome = append_entry(&root, "db talk", &["auth".into()], 500 * 1024, 30).unwrap();
        let text = std::fs::read_to_string(&outcome.file).unwrap();
        assert!(text.contains("### 🔄 Topic Switch [auth]"));
        assert!(text.contains("]*: db talk"));
        let plain = append_entry(&root, "no tags", &[], 500 * 1024, 30).unwrap();
        let text = std::fs::read_to_string(&plain.file).unwrap();
        assert_eq!(text.matches("Topic Switch").count(), 1);
    }

    #[test]
    fn append_rolls_over_the_limit_and_prunes_expired_files() {
        let root = scratch("rollover");
        // Fill past the injected limit so the next append rolls.
        append_entry(&root, &"x".repeat(200), &[], 100, 30).unwrap();
        let outcome = append_entry(&root, "after roll", &[], 100, 30).unwrap();
        assert!(outcome.rolled);
        let sessions = root.join(".NeuroStrata").join("sessions");
        let rolled: Vec<_> = std::fs::read_dir(&sessions)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("session-"))
            .collect();
        assert_eq!(rolled.len(), 1, "the rolled file survives inside retention");
        // Backdate it past retention and roll again: it must be pruned.
        let old = sessions.join(&rolled[0]);
        let old_time =
            std::time::SystemTime::now() - std::time::Duration::from_secs(60 * 60 * 24 * 90);
        let f = std::fs::File::options().write(true).open(&old).unwrap();
        f.set_modified(old_time).unwrap();
        append_entry(&root, &"y".repeat(200), &[], 100, 30).unwrap();
        assert!(!old.exists(), "rolled file past retention is pruned");
    }

    #[test]
    fn secret_scan_rejects_key_material() {
        let key = "ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdef123456";
        assert!(scan_for_append(key, &[]).is_some());
        assert!(scan_for_append("plain entry", &[]).is_none());
    }

    /// J22 (Task 2): a session file replaced by a symlink out of the project
    /// must refuse loudly and write nothing through the link — the buffer is
    /// an audit trail, and a silent write through a planted link would put
    /// session content wherever the link points.
    #[cfg(unix)]
    #[test]
    fn append_log_through_symlink_writes_nothing() {
        let root = scratch("symlink");
        let sessions = root.join(".NeuroStrata").join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let outside = scratch("symlink-target");
        let target = outside.join("loot.md");
        std::fs::write(&target, "original\n").unwrap();
        std::os::unix::fs::symlink(&target, sessions.join("current.md")).unwrap();

        let err = match append_entry(&root, "must not land", &[], 500 * 1024, 30) {
            Ok(_) => panic!("a symlinked session file is refused, not written through"),
            Err(e) => e,
        };
        assert!(
            err.to_string().contains("symlink"),
            "the refusal names the symlink: {err}"
        );
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "original\n",
            "the link target is untouched"
        );
    }

    /// J22 (Task 2 fix round / M-1): a `.NeuroStrata` symlink planted in
    /// the project, pointing outside it, must be refused BEFORE anything
    /// is created — the pre-fix order ran `create_dir_all` first, so the
    /// refusal was preceded by directory creation through the link, and
    /// its "nothing was written" was shy of literally true.
    #[cfg(unix)]
    #[test]
    fn append_log_through_sessions_symlink_creates_nothing() {
        let root = scratch("dirlink");
        let outside = scratch("dirlink-target");
        std::os::unix::fs::symlink(&outside, root.join(".NeuroStrata")).unwrap();

        let err = match append_entry(&root, "must not land", &[], 500 * 1024, 30) {
            Ok(_) => panic!("a sessions path through a planted link is refused"),
            Err(e) => e,
        };
        assert!(
            err.to_string().contains("outside"),
            "the refusal names the escape: {err}"
        );
        assert!(
            !outside.join("sessions").exists(),
            "no directory was created through the link"
        );
        assert!(
            std::fs::read_dir(&outside).unwrap().next().is_none(),
            "the link target gained nothing at all"
        );
    }

    /// J22 pin: the normal path still appends, entry after entry, into the
    /// project's own sessions directory.
    #[test]
    fn append_log_normal_path_appends() {
        let root = scratch("normal");
        append_entry(&root, "first entry", &[], 500 * 1024, 30).unwrap();
        let outcome = append_entry(&root, "second entry", &[], 500 * 1024, 30).unwrap();
        let text = std::fs::read_to_string(&outcome.file).unwrap();
        assert!(text.contains("first entry"), "the first entry persists: {text}");
        assert!(
            text.contains("second entry"),
            "the second entry appends: {text}"
        );
        assert!(
            outcome.file.starts_with(&root.display().to_string()),
            "the buffer file lives under the project root: {}",
            outcome.file
        );
    }

    #[tokio::test]
    async fn disabled_mode_is_a_no_op() {
        let root = scratch("off");
        let store = Arc::new(MockStore::new());
        let emb = Arc::new(StubEmbedder);
        let result = handle_append_log(
            json!({
                "content": "hello",
                "project_root": root.display().to_string(),
            }),
            emb,
            store,
        )
        .await;
        // Whether this no-ops depends on the machine's config; when disabled the
        // file must not exist, and when enabled the append must succeed. Both
        // outcomes are contract; only a crash is a failure.
        if result.contains("Episodic Buffer disabled") {
            assert!(!root
                .join(".NeuroStrata")
                .join("sessions")
                .join("current.md")
                .exists());
        } else {
            assert!(result.contains("\"appended\":true"));
        }
    }

    #[tokio::test]
    async fn a_tagged_turn_recalls_the_matching_fact() {
        let root = scratch("recall-match");
        let store = Arc::new(MockStore::new());
        let emb = Arc::new(StubEmbedder);
        store.init("probe").await.unwrap();

        // Store a fact with domain "Database Access" (normalizes to "database-access")
        let fact_id = "fact-1";
        let payload = crate::traits::MemoryPayload {
            content: "Use connection pooling for database access".to_string(),
            user_id: "tester".to_string(),
            memory_type: "fact".to_string(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: serde_json::json!({ "domain": "Database Access" }),
        };
        store
            .upsert("probe", fact_id, vec![0.1, 0.2, 0.3, 0.4], payload.clone())
            .await
            .unwrap();

        let result = handle_append_log(
            json!({
                "content": "working on db",
                "project_root": root.display().to_string(),
                "tags": ["database-access"],
                "namespace": "probe",
            }),
            emb.clone(),
            store.clone(),
        )
        .await;

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert!(parsed.get("appended").and_then(|v| v.as_bool()).unwrap_or(false));
        let recalled = parsed.get("recalled").expect("recalled field must be present");
        assert!(recalled.is_array());
        let recalled_arr = recalled.as_array().unwrap();
        assert_eq!(recalled_arr.len(), 1, "exactly one fact recalled: {}", result);
        assert_eq!(recalled_arr[0].get("id").and_then(|v| v.as_str()), Some(fact_id));
        assert_eq!(parsed.get("recalled_from").and_then(|v| v.as_str()), Some("domain"));
    }

    #[tokio::test]
    async fn the_recall_never_exceeds_three_entries_or_a_120_character_snippet() {
        let root = scratch("recall-cap");
        let store = Arc::new(MockStore::new());
        let emb = Arc::new(StubEmbedder);
        store.init("probe").await.unwrap();

        // Store 4 facts with long content
        for i in 1..=4 {
            let fact_id = format!("fact-{}", i);
            let long_content = "x".repeat(200);
            let payload = crate::traits::MemoryPayload {
                content: long_content,
                user_id: "tester".to_string(),
                memory_type: "fact".to_string(),
                agent_name: None,
                location: String::new(),
                location_lines: String::new(),
                metadata: serde_json::json!({ "domain": "database-access" }),
            };
            store
                .upsert("probe", &fact_id, vec![0.1, 0.2, 0.3, 0.4], payload)
                .await
                .unwrap();
        }

        let result = handle_append_log(
            json!({
                "content": "working on db",
                "project_root": root.display().to_string(),
                "tags": ["database-access"],
                "namespace": "probe",
            }),
            emb.clone(),
            store.clone(),
        )
        .await;

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        let recalled = parsed.get("recalled").expect("recalled field must be present");
        let recalled_arr = recalled.as_array().unwrap();
        assert!(recalled_arr.len() <= 3, "at most 3 entries recalled, got {}", recalled_arr.len());
        for entry in recalled_arr {
            let snippet = entry.get("snippet").and_then(|v| v.as_str()).unwrap_or("");
            assert!(
                snippet.chars().count() <= 120,
                "snippet exceeds 120 chars: {} (len={})",
                snippet,
                snippet.chars().count()
            );
        }
    }

    #[tokio::test]
    async fn an_untagged_turn_recalls_nothing_and_costs_no_embedding() {
        let root = scratch("recall-untagged");
        let store = Arc::new(MockStore::new());
        let calls = Arc::new(AtomicUsize::new(0));
        let emb = Arc::new(CountingEmbedder { calls: calls.clone() });
        store.init("probe").await.unwrap();

        // Store a fact
        let fact_id = "fact-1";
        let payload = crate::traits::MemoryPayload {
            content: "some fact".to_string(),
            user_id: "tester".to_string(),
            memory_type: "fact".to_string(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: serde_json::json!({ "domain": "database-access" }),
        };
        store
            .upsert("probe", fact_id, vec![0.1, 0.2, 0.3, 0.4], payload)
            .await
            .unwrap();

        let result = handle_append_log(
            json!({
                "content": "working on something",
                "project_root": root.display().to_string(),
                "namespace": "probe",
                // no tags
            }),
            emb.clone(),
            store.clone(),
        )
        .await;

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert!(parsed.get("appended").and_then(|v| v.as_bool()).unwrap_or(false));
        let recalled = parsed.get("recalled").expect("recalled field must be present");
        assert_eq!(recalled.as_array().unwrap().len(), 0, "recalled must be empty array");
        // No recalled_from key at all when no search ran
        assert!(
            parsed.get("recalled_from").is_none(),
            "recalled_from must be absent when no search ran: {}",
            result
        );
        // Zero embed calls for untagged turn
        assert_eq!(calls.load(Ordering::SeqCst), 0, "untagged turn must not call embed");
    }

    #[tokio::test]
    async fn a_tag_that_normalizes_to_nothing_is_dropped() {
        let root = scratch("recall-normalize-nothing");
        let store = Arc::new(MockStore::new());
        let emb = Arc::new(StubEmbedder);
        store.init("probe").await.unwrap();

        // Store a fact
        let fact_id = "fact-1";
        let payload = crate::traits::MemoryPayload {
            content: "some fact".to_string(),
            user_id: "tester".to_string(),
            memory_type: "fact".to_string(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: serde_json::json!({ "domain": "database-access" }),
        };
        store
            .upsert("probe", fact_id, vec![0.1, 0.2, 0.3, 0.4], payload)
            .await
            .unwrap();

        // Tags that normalize to nothing
        let result = handle_append_log(
            json!({
                "content": "working on something",
                "project_root": root.display().to_string(),
                "tags": ["!!!", "  "],
                "namespace": "probe",
            }),
            emb.clone(),
            store.clone(),
        )
        .await;

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        let recalled = parsed.get("recalled").expect("recalled field must be present");
        assert_eq!(recalled.as_array().unwrap().len(), 0, "recalled must be empty array");
        assert!(
            parsed.get("recalled_from").is_none(),
            "recalled_from must be absent when tags normalize to nothing: {}",
            result
        );
    }

    #[tokio::test]
    async fn the_recall_never_moves_an_access_count() {
        let root = scratch("recall-no-access-count");
        let store = Arc::new(MockStore::new());
        let emb = Arc::new(StubEmbedder);
        store.init("probe").await.unwrap();

        // Store a fact
        let fact_id = "fact-1";
        let payload = crate::traits::MemoryPayload {
            content: "some fact".to_string(),
            user_id: "tester".to_string(),
            memory_type: "fact".to_string(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: serde_json::json!({ "domain": "database-access" }),
        };
        store
            .upsert("probe", fact_id, vec![0.1, 0.2, 0.3, 0.4], payload)
            .await
            .unwrap();

        let initial_increments = store.incremented_ids().len();

        let result = handle_append_log(
            json!({
                "content": "working on db",
                "project_root": root.display().to_string(),
                "tags": ["database-access"],
                "namespace": "probe",
            }),
            emb.clone(),
            store.clone(),
        )
        .await;

        let final_increments = store.incremented_ids().len();
        assert_eq!(
            final_increments, initial_increments,
            "increment_access_count must not be called during recall; got {} new increments",
            final_increments - initial_increments
        );

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert!(parsed.get("appended").and_then(|v| v.as_bool()).unwrap_or(false));
    }

    #[tokio::test]
    async fn a_failed_search_does_not_fail_the_append() {
        let root = scratch("recall-failed-search");
        let store = Arc::new(MockStore::new());
        let emb = Arc::new(StubEmbedder);
        store.init("probe").await.unwrap();
        // Make search_with_domain fail
        store.set_search_fails(true);

        let result = handle_append_log(
            json!({
                "content": "working on db",
                "project_root": root.display().to_string(),
                "tags": ["database-access"],
                "namespace": "probe",
            }),
            emb.clone(),
            store.clone(),
        )
        .await;

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert!(parsed.get("appended").and_then(|v| v.as_bool()).unwrap_or(false));
        let recalled = parsed.get("recalled").expect("recalled field must be present");
        assert_eq!(recalled.as_array().unwrap().len(), 0, "recalled must be empty array on error");
        assert!(
            parsed.get("recalled_from").is_none(),
            "recalled_from must be absent when search failed: {}",
            result
        );
    }

    #[tokio::test]
    async fn an_empty_namespace_falls_back_to_project_root() {
        let root = scratch("recall-empty-ns");
        // The fallback namespace is the project root's own directory name.
        let fallback_ns = root
            .file_name()
            .and_then(|n| n.to_str())
            .expect("scratch directory has a name")
            .to_string();
        let store = Arc::new(MockStore::new());
        let emb = Arc::new(StubEmbedder);
        store.init(&fallback_ns).await.unwrap();

        // Store a fact in the FALLBACK namespace, with a domain that matches the tag.
        // A fact in some other namespace cannot prove the fallback happened: the
        // mock filters by namespace, so only the fallback's own row comes back.
        let fact_id = "fact-fallback-ns";
        let payload = crate::traits::MemoryPayload {
            content: "some fact".to_string(),
            user_id: "tester".to_string(),
            memory_type: "fact".to_string(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: serde_json::json!({ "domain": "database-access" }),
        };
        store
            .upsert(&fallback_ns, fact_id, vec![0.1, 0.2, 0.3, 0.4], payload)
            .await
            .unwrap();

        // namespace: "" is treated as absent, so the search runs against the
        // project_root file_name fallback.
        let result = handle_append_log(
            json!({
                "content": "working on something",
                "project_root": root.display().to_string(),
                "tags": ["database-access"],
                "namespace": "",  // empty string -> treated as absent -> fallback
            }),
            emb.clone(),
            store.clone(),
        )
        .await;

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert!(parsed.get("appended").and_then(|v| v.as_bool()).unwrap_or(false));
        let recalled = parsed.get("recalled").expect("recalled field must be present");
        assert!(recalled.is_array());
        let recalled_arr = recalled.as_array().unwrap();
        // The row seeded in the fallback namespace is the one that must come back.
        // Under the old behaviour the namespace resolved to "" and no row matched,
        // so this assertion is what makes the test able to fail.
        assert!(
            recalled_arr
                .iter()
                .any(|entry| entry.get("id").and_then(|v| v.as_str()) == Some(fact_id)),
            "the fact seeded in the fallback namespace must be recalled: {}",
            result
        );
        // recalled_from is present because a search ran, and the domain filter
        // chose the rows.
        assert!(
            parsed.get("recalled_from").is_some(),
            "recalled_from must be present when search ran (even against empty namespace): {}",
            result
        );
        assert_eq!(parsed.get("recalled_from").and_then(|v| v.as_str()), Some("domain"));
    }

    #[tokio::test]
    async fn exactly_one_embedding_call_per_tagged_turn() {
        let root = scratch("recall-one-embed");
        let store = Arc::new(MockStore::new());
        let calls = Arc::new(AtomicUsize::new(0));
        let emb = Arc::new(CountingEmbedder { calls: calls.clone() });
        store.init("probe").await.unwrap();

        // Store a fact
        let fact_id = "fact-1";
        let payload = crate::traits::MemoryPayload {
            content: "some fact".to_string(),
            user_id: "tester".to_string(),
            memory_type: "fact".to_string(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: serde_json::json!({ "domain": "database-access" }),
        };
        store
            .upsert("probe", fact_id, vec![0.1, 0.2, 0.3, 0.4], payload)
            .await
            .unwrap();

        let result = handle_append_log(
            json!({
                "content": "working on db",
                "project_root": root.display().to_string(),
                "tags": ["database-access"],
                "namespace": "probe",
            }),
            emb.clone(),
            store.clone(),
        )
        .await;

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert!(parsed.get("appended").and_then(|v| v.as_bool()).unwrap_or(false));
        // Exactly one embed call for a tagged turn
        assert_eq!(calls.load(Ordering::SeqCst), 1, "exactly one embed call for tagged turn");
    }

    #[tokio::test]
    async fn append_failure_reports_honestly() {
        let root = scratch("append-fail");
        // Create a regular file where .NeuroStrata directory needs to be created
        let neurostrata_file = root.join(".NeuroStrata");
        std::fs::write(&neurostrata_file, "block").unwrap();

        let store = Arc::new(MockStore::new());
        let emb = Arc::new(StubEmbedder);

        let result = handle_append_log(
            json!({
                "content": "working on something",
                "project_root": root.display().to_string(),
                "tags": ["database-access"],
            }),
            emb,
            store,
        )
        .await;

        // The machine's global config may disable the episodic buffer: the
        // handler then returns a plain string before the append is attempted.
        // Branch on that instead of parsing a string that is not JSON.
        if result.contains("Episodic Buffer disabled") {
            assert!(
                !result.contains("\"appended\":true"),
                "a disabled buffer must not claim a write: {}",
                result
            );
        } else {
            let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
            // F1: appended must be false
            assert_eq!(parsed.get("appended").and_then(|v| v.as_bool()), Some(false));
            // F1: error field with old wording
            let error = parsed.get("error").and_then(|v| v.as_str()).unwrap_or("");
            assert!(error.contains("could not append to the Episodic Buffer"), "error missing or wrong: {}", error);
            // F1: other keys present with empty/zero values
            assert_eq!(parsed.get("file").and_then(|v| v.as_str()), Some(""));
            assert_eq!(parsed.get("size_bytes").and_then(|v| v.as_u64()), Some(0));
            assert_eq!(parsed.get("rolled").and_then(|v| v.as_bool()), Some(false));
            // F1: recalled present (empty because search didn't run - append failed first)
            assert!(parsed.get("recalled").is_some());
            assert!(parsed.get("recalled").unwrap().as_array().unwrap().is_empty());
            // F1: no recalled_from because search didn't run
            assert!(parsed.get("recalled_from").is_none(), "recalled_from must be absent when no search ran: {}", result);
        }
    }

    #[tokio::test]
    async fn a_recalled_negative_exemplar_is_prefixed_and_bounded() {
        let root = scratch("recall-neg-prefix");
        let store = Arc::new(MockStore::new());
        let emb = Arc::new(StubEmbedder);
        store.init("probe").await.unwrap();

        let fact_id = "neg-1";
        let long_content = "x".repeat(200);
        let payload = crate::traits::MemoryPayload {
            content: long_content,
            user_id: "tester".to_string(),
            memory_type: "fact".to_string(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: serde_json::json!({
                "domain": "database-access",
                "attempt": {
                    "approach": "retry with backoff",
                    "outcome": "failed",
                    "failure_mode": "timeout",
                    "context": "under load"
                }
            }),
        };
        store
            .upsert("probe", fact_id, vec![0.1, 0.2, 0.3, 0.4], payload)
            .await
            .unwrap();

        let result = handle_append_log(
            json!({
                "content": "working on db",
                "project_root": root.display().to_string(),
                "tags": ["database-access"],
                "namespace": "probe",
            }),
            emb.clone(),
            store.clone(),
        )
        .await;

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        let recalled = parsed.get("recalled").unwrap().as_array().unwrap();
        assert_eq!(recalled.len(), 1);
        let snippet = recalled[0].get("snippet").and_then(|v| v.as_str()).unwrap();
        assert!(
            snippet.starts_with("[FAILED ATTEMPT] "),
            "snippet must start with the failed-attempt prefix: {}",
            snippet
        );
        assert!(
            snippet.chars().count() <= 120,
            "snippet exceeds 120 chars: {} (len={})",
            snippet,
            snippet.chars().count()
        );
    }

    #[tokio::test]
    async fn a_recalled_negative_exemplar_shorter_than_the_prefix_budget_keeps_the_prefix() {
        let root = scratch("recall-neg-short");
        let store = Arc::new(MockStore::new());
        let emb = Arc::new(StubEmbedder);
        store.init("probe").await.unwrap();

        let fact_id = "neg-short";
        let payload = crate::traits::MemoryPayload {
            content: "hi".to_string(),
            user_id: "tester".to_string(),
            memory_type: "fact".to_string(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: serde_json::json!({
                "domain": "database-access",
                "attempt": {
                    "approach": "retry with backoff",
                    "outcome": "failed",
                    "failure_mode": "timeout",
                    "context": "under load"
                }
            }),
        };
        store
            .upsert("probe", fact_id, vec![0.1, 0.2, 0.3, 0.4], payload)
            .await
            .unwrap();

        let result = handle_append_log(
            json!({
                "content": "working on db",
                "project_root": root.display().to_string(),
                "tags": ["database-access"],
                "namespace": "probe",
            }),
            emb.clone(),
            store.clone(),
        )
        .await;

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        let recalled = parsed.get("recalled").unwrap().as_array().unwrap();
        assert_eq!(recalled.len(), 1);
        let snippet = recalled[0].get("snippet").and_then(|v| v.as_str()).unwrap();
        assert!(
            snippet.starts_with("[FAILED ATTEMPT] "),
            "prefix must never be dropped: {}",
            snippet
        );
        assert!(
            snippet.chars().count() <= 120,
            "snippet exceeds 120 chars: {} (len={})",
            snippet,
            snippet.chars().count()
        );
    }

    #[tokio::test]
    async fn a_recalled_positive_fact_is_not_prefixed() {
        let root = scratch("recall-pos-no-prefix");
        let store = Arc::new(MockStore::new());
        let emb = Arc::new(StubEmbedder);
        store.init("probe").await.unwrap();

        let fact_id = "pos-1";
        let payload = crate::traits::MemoryPayload {
            content: "Use connection pooling for database access".to_string(),
            user_id: "tester".to_string(),
            memory_type: "fact".to_string(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: serde_json::json!({ "domain": "database-access" }),
        };
        store
            .upsert("probe", fact_id, vec![0.1, 0.2, 0.3, 0.4], payload)
            .await
            .unwrap();

        let result = handle_append_log(
            json!({
                "content": "working on db",
                "project_root": root.display().to_string(),
                "tags": ["database-access"],
                "namespace": "probe",
            }),
            emb.clone(),
            store.clone(),
        )
        .await;

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        let recalled = parsed.get("recalled").unwrap().as_array().unwrap();
        assert_eq!(recalled.len(), 1);
        let snippet = recalled[0].get("snippet").and_then(|v| v.as_str()).unwrap();
        assert!(
            !snippet.starts_with("[FAILED ATTEMPT] "),
            "positive fact must not be prefixed: {}",
            snippet
        );
    }
}
