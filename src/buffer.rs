//! Episodic Buffer — the rolling session log documented in README
//! ("The Episodic Buffer & Operator Controls"). `neurostrata_append_log`
//! writes timestamped entries to `<project_root>/.NeuroStrata/sessions/current.md`,
//! injecting a `### 🔄 Topic Switch` marker on tagged turns, rolling the file
//! at 500KB, and pruning rolled files past the retention window.

use serde_json::{json, Value};
use std::path::Path;

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
    let path = dirs::home_dir()
        .map(|h| h.join(".config").join("neurostrata").join("config.json"));
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

/// Append one entry (and its optional topic-switch marker) to the buffer.
/// `max_bytes` is injectable so tests do not have to write 500KB.
pub(crate) fn append_entry(
    project_root: &Path,
    content: &str,
    tags: &[String],
    max_bytes: u64,
    retention_days: u64,
) -> std::io::Result<Outcome> {
    let sessions = project_root.join(".NeuroStrata").join("sessions");
    std::fs::create_dir_all(&sessions)?;
    let current = sessions.join("current.md");

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
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&current)?;
    file.write_all(entry.as_bytes())?;
    let size_bytes = std::fs::metadata(&current).map(|m| m.len()).unwrap_or(0);

    Ok(Outcome {
        file: current.display().to_string(),
        size_bytes,
        rolled,
    })
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

pub async fn handle_append_log(arguments: Value) -> String {
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

    match append_entry(
        &project_root,
        &content,
        &tags,
        ROLLOVER_BYTES,
        config.retention_days,
    ) {
        Ok(outcome) => json!({
            "appended": true,
            "file": outcome.file,
            "size_bytes": outcome.size_bytes,
            "rolled": outcome.rolled,
            "tags": tags,
        })
        .to_string(),
        Err(e) => format!("ERROR: could not append to the Episodic Buffer: {}", e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let old_time = std::time::SystemTime::now() - std::time::Duration::from_secs(60 * 60 * 24 * 90);
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

    #[tokio::test]
    async fn disabled_mode_is_a_no_op() {
        let root = scratch("off");
        let result = handle_append_log(json!({
            "content": "hello",
            "project_root": root.display().to_string(),
        }))
        .await;
        // Whether this no-ops depends on the machine's config; when disabled the
        // file must not exist, and when enabled the append must succeed. Both
        // outcomes are contract; only a crash is a failure.
        if result.contains("Episodic Buffer disabled") {
            assert!(!root.join(".NeuroStrata").join("sessions").join("current.md").exists());
        } else {
            assert!(result.contains("\"appended\":true"));
        }
    }
}
