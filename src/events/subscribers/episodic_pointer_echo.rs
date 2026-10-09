//! `EpisodicPointerEcho` -- the Continuous Backup Protocol, automated.
//!
//! Until now the episodic buffer only ever heard from the agent: a turn wrote
//! a line because the agent remembered to call `neurostrata_append_log`, and
//! a turn that forgot wrote nothing. This subscriber closes that gap by
//! listening to the bus instead: every `Created` leaves a one-line pointer in
//! the same file the agent's own entries land in, so the buffer is the union
//! of what was written and what was remembered.
//!
//! It is a pointer, not a copy. The memory itself is already in the store, and
//! the buffer's job is to say it exists, when, and where -- which is also why
//! the line can be deduplicated on `(id, kind)` and never grows with the
//! memory's content.

use crate::buffer;
use crate::events::{
    MemorySubscriber, RecursionToken, SubscriberContext, SubscriberError, ThalamicPulse,
};
use async_trait::async_trait;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Marks a line as machine-written, so a reader can tell an echo from a note
/// the agent wrote by hand -- and so [`pointer_matches`] can. `append_entry`
/// prepends `- *[stamp]*: `, so on a written line the label is the *tail* of
/// field 0, not a field of its own; that is the shape check.
const POINTER_LABEL: &str = "pointer echo";

/// How far back the idempotency scan looks, in lines.
///
/// A safety bound, not a tuning knob: the buffer rolls at 500KB -- thousands of
/// lines of markdown -- so in practice the scan sees the whole file. It exists
/// so a pathologically long buffer cannot turn every `Created` into a
/// full-file search.
const IDEMPOTENCY_SCAN_LINES: usize = 1000;

/// How far back the idempotency scan looks, in bytes.
///
/// A pointer line is ~60 bytes, so this holds several times the line window:
/// whichever bound binds first, the answer is the same. Read from the end of
/// the file rather than whole, because the check runs on the dispatcher's task
/// and a rolled buffer is 500KB.
const IDEMPOTENCY_TAIL_BYTES: u64 = 128 * 1024;

/// Mirrors `buffer`'s rollover size. `append_entry` takes it as a parameter so
/// tests need not write 500KB; it is not configurable, so there is nothing to
/// read it from.
const ROLLOVER_BYTES: u64 = 500 * 1024;

/// Appends a pointer line to the episodic buffer for every `Created` pulse.
///
/// The project root is fixed at construction, because the buffer lives under
/// the project the agent is working in and a subscriber may not be handed a
/// path that disagrees with the write it is reacting to.
pub struct EpisodicPointerEcho {
    project_root: PathBuf,
    /// The operator's episodic-buffer settings, read fresh on every `Created`.
    ///
    /// A function pointer rather than a call to `buffer::load_config` inline so
    /// a test can supply a *disabled* buffer: `load_config` reads the machine's
    /// `~/.config/neurostrata/config.json`, and CI machines differ on it, so a
    /// test written against the real thing passes or fails depending on who ran
    /// it. [`Self::new`] wires the real loader, so the shipped path is the same
    /// read `neurostrata_append_log` makes.
    load_config: fn() -> buffer::BufferConfig,
}

impl EpisodicPointerEcho {
    /// `project_root` is the same root `neurostrata_append_log` is given: the
    /// buffer lives at `<project_root>/.NeuroStrata/sessions/`.
    pub fn new(project_root: impl Into<PathBuf>) -> Self {
        Self { project_root: project_root.into(), load_config: buffer::load_config }
    }

    /// The subscriber over an explicit config loader. Tests only: the operator
    /// switch has two outcomes and both are contract, so both must be assertable
    /// without editing a machine-global file.
    #[cfg(test)]
    fn with_config(
        project_root: impl Into<PathBuf>,
        load_config: fn() -> buffer::BufferConfig,
    ) -> Self {
        Self { project_root: project_root.into(), load_config }
    }
}

#[async_trait]
impl MemorySubscriber for EpisodicPointerEcho {
    fn name(&self) -> &'static str {
        "episodic_pointer_echo"
    }

    async fn handle(
        &self,
        event: &ThalamicPulse,
        _token: &RecursionToken,
        _ctx: &SubscriberContext<'_>,
    ) -> Result<(), SubscriberError> {
        let ThalamicPulse::Created { id, namespace, kind } = event else {
            return Ok(());
        };

        // The operator's switch, read once and used for both the decision and
        // the write's retention, so a config change cannot land between them.
        let config = (self.load_config)();
        if !config.enabled {
            tracing::debug!(
                subscriber = self.name(),
                "episodic buffer disabled by config; no pointer written"
            );
            return Ok(());
        }

        let buffer_file = current_log(&self.project_root);
        if already_echoed(&buffer_file, id, kind) {
            tracing::debug!(
                subscriber = self.name(),
                id,
                "pointer already in the episodic buffer; skipping the echo"
            );
            return Ok(());
        }

        buffer::append_entry(
            &self.project_root,
            &format!("{POINTER_LABEL}\t{id}\t{kind}\t{namespace}"),
            &[],
            ROLLOVER_BYTES,
            config.retention_days,
        )?;
        Ok(())
    }
}

/// The file `buffer::append_entry` appends to.
///
/// Spelled out rather than taken from the `Outcome` that write returns,
/// because the idempotency check has to run *before* the write it might skip.
/// This is the one place the subscriber mirrors `buffer`'s layout, and it must
/// move if that moves.
fn current_log(project_root: &Path) -> PathBuf {
    project_root.join(".NeuroStrata").join("sessions").join("current.md")
}

/// Whether the buffer already carries a pointer for this `(id, kind)`.
///
/// Two files, because rollover moves the answer: `append_entry` renames
/// `current.md` to `session-<stamp>.md` and starts a new, empty `current.md`,
/// so a re-emitted pair that predates a roll is found in the rolled file and
/// not in the current one. Scanning only the current file is exactly the
/// duplicate the check exists to prevent.
///
/// The rolled scan is deliberately limited to the newest rolled file: the check
/// errs toward writing, never toward suppressing. A duplicated pointer line is
/// cosmetic; a suppressed one is a hole in the audit trail, so a pointer that
/// has fallen two rolls back is an accepted boundary of this heuristic.
///
/// An unreadable or absent file is not a duplicate: the point of the check is
/// to stop a second echo, not to vouch for the file, and the write that
/// follows surfaces any real I/O problem.
fn already_echoed(buffer_file: &Path, id: &str, kind: &str) -> bool {
    if file_carries_pointer(buffer_file, id, kind) {
        return true;
    }
    newest_rolled_log(buffer_file.parent())
        .is_some_and(|rolled| file_carries_pointer(&rolled, id, kind))
}

/// The newest `session-*.md` in the sessions directory, if the buffer has rolled.
///
/// Ordered by mtime, then by name so two rolls landing in the same clock tick
/// still resolve deterministically. An unstattable entry is skipped: a rolled
/// file the filesystem cannot describe is one the scan cannot read either.
fn newest_rolled_log(sessions: Option<&Path>) -> Option<PathBuf> {
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(sessions?).ok()?.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("session-") || !name.ends_with(".md") {
            continue;
        }
        let Ok(mtime) = entry.metadata().and_then(|m| m.modified()) else {
            continue;
        };
        let path = entry.path();
        let is_newer = newest.as_ref().is_none_or(|(t, p)| (mtime, &path) > (*t, p));
        if is_newer {
            newest = Some((mtime, path));
        }
    }
    newest.map(|(_, path)| path)
}

/// Whether the tail of one buffer file carries a pointer to this `(id, kind)`.
///
/// Reads the tail rather than the file: this runs on the dispatcher's task once
/// per `Created`, and a rolled buffer is 500KB.
fn file_carries_pointer(path: &Path, id: &str, kind: &str) -> bool {
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let Ok(len) = file.metadata().map(|m| m.len()) else {
        return false;
    };
    let start = len.saturating_sub(IDEMPOTENCY_TAIL_BYTES);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return false;
    }
    let mut buf = Vec::new();
    if file.read_to_end(&mut buf).is_err() {
        return false;
    }
    let chunk = String::from_utf8_lossy(&buf);
    let chunk: &str = &chunk;
    // Started mid-file: the first line is a fragment, and may have split a
    // multi-byte character (hence the lossy decode above). Dropped, because a
    // half-line matching nothing is the safe way to be wrong.
    let tail = if start > 0 { chunk.split_once('\n').map_or("", |(_, rest)| rest) } else { chunk };
    tail.lines().rev().take(IDEMPOTENCY_SCAN_LINES).any(|line| pointer_matches(line, id, kind))
}

/// Whether one buffer line is a pointer to exactly this `(id, kind)`.
///
/// Positional, not a search: field 0 must end in [`POINTER_LABEL`] -- the
/// marker only the subscriber writes -- and the next two fields must be the
/// `id` and the `kind`, in that order. A line the agent wrote by hand can carry
/// any tab pair it likes; it cannot end field 0 with the machine's label, so it
/// can never suppress an echo. Comparing the pair positionally (rather than
/// for a substring) is also what keeps an id that is a prefix of another (`a`
/// inside `xa`) from reading as a duplicate.
fn pointer_matches(line: &str, id: &str, kind: &str) -> bool {
    let mut fields = line.split('\t');
    let labeled = fields.next().is_some_and(|prefix| prefix.ends_with(POINTER_LABEL));
    labeled && fields.next() == Some(id) && fields.next() == Some(kind)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::ThalamicBus;
    use std::time::Duration;

    /// A fresh project root per test, named after the test so a leftover from
    /// an earlier run can never be read back as this run's output.
    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "neurostrata-pointer-echo-test-{}-{}",
            std::process::id(),
            tag
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The buffer file the agent's own `append_log` entries land in.
    fn buffer_file(root: &Path) -> PathBuf {
        root.join(".NeuroStrata").join("sessions").join("current.md")
    }

    fn created(id: &str, kind: &str) -> ThalamicPulse {
        ThalamicPulse::Created {
            id: id.to_string(),
            namespace: "NeuroStrata".to_string(),
            kind: kind.to_string(),
        }
    }

    /// The operator switch on, which is every test's default. See
    /// [`EpisodicPointerEcho::with_config`]: the real config is machine-global,
    /// so tests that want a definite outcome supply it.
    fn buffer_on() -> buffer::BufferConfig {
        buffer::BufferConfig { enabled: true, retention_days: 30 }
    }

    /// The operator switch off -- `"episodic_buffer": false` in the config.
    fn buffer_off() -> buffer::BufferConfig {
        buffer::BufferConfig { enabled: false, retention_days: 30 }
    }

    fn echo_of(root: &Path) -> EpisodicPointerEcho {
        EpisodicPointerEcho::with_config(root.to_path_buf(), buffer_on)
    }

    /// Long enough for the dispatcher to pop the queue and run every
    /// subscriber against the pulse, as the bus's own tests do.
    const DISPATCH_WAIT: Duration = Duration::from_millis(50);

    #[tokio::test]
    async fn created_pulse_leaves_a_pointer_line_in_the_buffer() {
        let root = scratch("emits");
        let bus = ThalamicBus::new(16);
        bus.register(Box::new(echo_of(&root)));

        bus.emit(created("mem-1", "task"), &RecursionToken::root());
        tokio::time::sleep(DISPATCH_WAIT).await;

        let text = std::fs::read_to_string(buffer_file(&root))
            .expect("a Created pulse must leave a buffer file behind");
        assert_eq!(text.lines().count(), 1, "exactly one pointer per Created: {text:?}");
        assert!(text.contains("mem-1\ttask"), "the id/kind pointer: {text:?}");
        assert!(text.contains("NeuroStrata"), "the namespace it landed in: {text:?}");
        assert!(text.starts_with("- *["), "written through the buffer's own entry format: {text:?}");
    }

    /// The failure path, drilled: the same `(id, kind)` emitted twice must
    /// leave one line. Without the idempotency check the second emit appends a
    /// duplicate, and every retry of a write -- a reconnect, a replayed
    /// extraction -- would double-count it in the audit trail.
    #[tokio::test]
    async fn repeated_created_is_echoed_once() {
        let root = scratch("idempotent");
        let bus = ThalamicBus::new(16);
        bus.register(Box::new(echo_of(&root)));

        bus.emit(created("mem-1", "task"), &RecursionToken::root());
        tokio::time::sleep(DISPATCH_WAIT).await;
        bus.emit(created("mem-1", "task"), &RecursionToken::root());
        tokio::time::sleep(DISPATCH_WAIT).await;

        let text = std::fs::read_to_string(buffer_file(&root)).unwrap();
        assert_eq!(
            text.lines().count(),
            1,
            "a second emit of the same (id, kind) must be a no-op: {text:?}"
        );
        assert_eq!(text.matches("mem-1\ttask").count(), 1, "one pointer, one occurrence: {text:?}");
        // Both pulses really did reach the dispatcher -- the count above is the
        // idempotency check holding, not a second pulse silently lost.
        let m = bus.metrics();
        assert_eq!(m.events_emitted, 2, "both emits accepted: {m:?}");
        assert_eq!(m.dispatcher_handled, 2, "both pulses dispatched: {m:?}");
    }

    /// A different memory of the same kind is a different pointer: the key is
    /// the pair, so one must not silence the other.
    #[tokio::test]
    async fn a_different_id_is_echoed_too() {
        let root = scratch("distinct");
        let bus = ThalamicBus::new(16);
        bus.register(Box::new(echo_of(&root)));

        bus.emit(created("mem-1", "task"), &RecursionToken::root());
        bus.emit(created("mem-2", "task"), &RecursionToken::root());
        tokio::time::sleep(DISPATCH_WAIT).await;

        let text = std::fs::read_to_string(buffer_file(&root)).unwrap();
        assert_eq!(text.lines().count(), 2, "distinct ids are distinct pointers: {text:?}");
    }

    /// Only `Created` echoes. `Superseded` and `Archived` are mutations of a
    /// memory that already has a pointer; echoing them would put a second line
    /// per memory in a buffer whose value is one pointer per memory.
    #[tokio::test]
    async fn non_created_pulses_write_nothing() {
        let root = scratch("other");
        let bus = ThalamicBus::new(16);
        bus.register(Box::new(echo_of(&root)));

        bus.emit(
            ThalamicPulse::Superseded {
                old_id: "mem-1".into(),
                new_id: "mem-2".into(),
                namespace: "NeuroStrata".into(),
            },
            &RecursionToken::root(),
        );
        bus.emit(
            ThalamicPulse::Archived { id: "mem-1".into(), namespace: "NeuroStrata".into() },
            &RecursionToken::root(),
        );
        tokio::time::sleep(DISPATCH_WAIT).await;

        assert!(
            !buffer_file(&root).exists(),
            "a pulse the subscriber does not match must not create the buffer"
        );
        assert_eq!(bus.metrics().dispatcher_handled, 2, "both pulses still dispatched");
    }

    /// `"episodic_buffer": false` means the operator turned this file off. The
    /// echo is a write to that file, so it has to obey the same switch --
    /// otherwise the one setting an operator reaches for to stop episodic
    /// logging is silently ignored by every `Created` the bus sees.
    #[tokio::test]
    async fn a_disabled_buffer_writes_no_pointer() {
        let root = scratch("disabled");
        let bus = ThalamicBus::new(16);
        bus.register(Box::new(EpisodicPointerEcho::with_config(root.clone(), buffer_off)));

        bus.emit(created("mem-1", "task"), &RecursionToken::root());
        tokio::time::sleep(DISPATCH_WAIT).await;

        assert!(
            !buffer_file(&root).exists(),
            "a disabled episodic buffer must not be written by the subscriber"
        );
        // Suppressed at the write, not by dropping the pulse: the dispatcher
        // still handled it, which is what makes this an operator's switch
        // rather than a quiet failure.
        assert_eq!(bus.metrics().dispatcher_handled, 1, "the pulse still reached the subscriber");
    }

    /// Idempotency across a rollover. The buffer renames `current.md` to
    /// `session-<stamp>.md` and starts a new one, so after a roll the pair is
    /// only in the rolled file: a check that read `current.md` alone would see
    /// an empty buffer and write the pointer a second time -- which is the
    /// duplicate the check exists to prevent.
    #[tokio::test]
    async fn idempotency_survives_a_rollover() {
        let root = scratch("rollover");
        let bus = ThalamicBus::new(16);
        bus.register(Box::new(echo_of(&root)));

        bus.emit(created("mem-1", "task"), &RecursionToken::root());
        tokio::time::sleep(DISPATCH_WAIT).await;

        // Roll the way the buffer does -- through its own write path, with an
        // entry past the limit, so the rename is the real one.
        buffer::append_entry(
            &root,
            &"x".repeat(ROLLOVER_BYTES as usize),
            &[],
            ROLLOVER_BYTES,
            30,
        )
        .expect("the filler entry rolls the buffer");

        bus.emit(created("mem-1", "task"), &RecursionToken::root());
        tokio::time::sleep(DISPATCH_WAIT).await;

        let sessions = root.join(".NeuroStrata").join("sessions");
        let rolled = newest_rolled_log(Some(&sessions)).expect("the append rolled current.md");
        let rolled_text = std::fs::read_to_string(&rolled).unwrap();
        assert!(
            rolled_text.contains("mem-1\ttask"),
            "the pre-roll pointer is in the rolled file: {rolled_text:?}"
        );

        let current = std::fs::read_to_string(buffer_file(&root)).unwrap();
        assert!(
            !current.contains("mem-1\ttask"),
            "the rolled pointer must not be echoed again into the new current.md: {current:?}"
        );

        // The scratch dir is pid-scoped, so nothing later would ever reclaim the
        // 500KB filler.
        let _ = std::fs::remove_dir_all(&root);
    }

    /// An agent-authored line that happens to carry the same tab pair. Matching
    /// on the pair alone read this as a pointer and silenced the echo forever,
    /// leaving the memory with no pointer at all; requiring the machine's label
    /// is what separates the two.
    #[tokio::test]
    async fn an_agent_note_does_not_suppress_the_echo() {
        let root = scratch("agent-note");
        let bus = ThalamicBus::new(16);
        bus.register(Box::new(echo_of(&root)));

        buffer::append_entry(
            &root,
            "reviewing\tmem-1\ttask\tbefore the standup",
            &[],
            ROLLOVER_BYTES,
            30,
        )
        .expect("the agent's own entry lands in the same file");
        bus.emit(created("mem-1", "task"), &RecursionToken::root());
        tokio::time::sleep(DISPATCH_WAIT).await;

        let text = std::fs::read_to_string(buffer_file(&root)).unwrap();
        assert_eq!(
            text.matches(&format!("{POINTER_LABEL}\tmem-1\ttask")).count(),
            1,
            "the echo follows the agent's note instead of being suppressed by it: {text:?}"
        );
        assert!(
            text.contains("before the standup"),
            "the agent's entry is untouched: {text:?}"
        );
    }

    /// The production constructor, checked against the machine it runs on. The
    /// rest of the suite injects its own loader, so nothing else would notice a
    /// `new()` that wired a stub instead of `buffer::load_config` -- and the
    /// operator's switch would then be honored by the tool and ignored by the
    /// subscriber. Written against `buffer::load_config()`'s live answer, so the
    /// assertion holds on a machine that has the buffer on and on one that has
    /// turned it off.
    #[tokio::test]
    async fn the_production_constructor_follows_the_operator_config() {
        let root = scratch("production-config");
        let bus = ThalamicBus::new(16);
        bus.register(Box::new(EpisodicPointerEcho::new(root.clone())));

        bus.emit(created("mem-1", "task"), &RecursionToken::root());
        tokio::time::sleep(DISPATCH_WAIT).await;

        let written = buffer_file(&root).exists();
        assert_eq!(
            written,
            buffer::load_config().enabled,
            "the subscriber writes exactly when the operator's config enables the buffer"
        );
        if written {
            let text = std::fs::read_to_string(buffer_file(&root)).unwrap();
            assert!(text.contains("mem-1\ttask"), "the pointer it wrote: {text:?}");
        }
    }

    /// The substring trap the positional match exists to avoid: `mem` is a
    /// prefix of the id `mem-1`, so a `contains("mem")`-style check would read
    /// the existing pointer for `mem-1` as one for `mem`.
    #[test]
    fn a_prefix_id_is_not_mistaken_for_a_duplicate() {
        let line = format!("- *[2026-10-09 08:00:00]*: {POINTER_LABEL}\tmem-1\ttask\tNeuroStrata");
        assert!(!pointer_matches(&line, "mem", "task"), "suffix ids are distinct: {line:?}");
        assert!(pointer_matches(&line, "mem-1", "task"), "the exact pair matches: {line:?}");
        assert!(!pointer_matches(&line, "mem-1", "fact"), "the kind is half the key: {line:?}");
        assert!(
            !pointer_matches("- *[2026-10-09 08:00:00]*: mem-1 is the task I finished", "mem-1", "task"),
            "an agent-authored note is not a pointer: no tabbed fields"
        );
        assert!(
            !pointer_matches(
                "- *[2026-10-09 08:00:00]*: reviewing\tmem-1\ttask\tbefore the standup",
                "mem-1",
                "task"
            ),
            "an agent-authored note carrying the pair is still not a pointer: field 0 lacks the label"
        );
    }
}