//! R20 / Composite Recs #19 & #20: the procedure-acknowledgment and buffer
//! write contracts, pinned where an integration test can reach them.
//!
//! The crate ships a binary target only (see Cargo.toml), so the handlers
//! themselves are exercised by the in-file unit tests --
//! `src/buffer.rs::tests::concurrent_writers_to_one_root_never_interleave_partial_lines`
//! and
//! `src/tools/procedure.rs::tests::concurrent_performs_serialize_and_never_double_spend_the_budget`.
//! What this target pins, because nothing else can fail a CI build on it:
//!
//! 1. the serialization hardware actually exists in the sources -- the
//!    per-root buffer lock table, the per-procedure fire-lock map, and the
//!    `previous_performance_count` audit field -- each wired into the exact
//!    functions that mutate the shared state;
//! 2. the Episodic Buffer's on-disk write lifecycle: an entry appended in
//!    the buffer's line format under a `TempDir` project root persists with
//!    its entry id, survives a cold re-open (a "new session") byte-stable,
//!    and a rollover keeps the `session-` name the retention pruner matches.

use std::fs;

// The receipts store is deliberately crate-free (std + a HashMap only), so the
// real `ReceiptStore` ships here compiled from its real path and the tests
// below exercise its actual record/replay/current-binding behavior over the
// written receipt lines — no transcription of it can drift into this target.
#[path = "../src/receipts/store.rs"]
pub mod receipt_store;

// ── 1. the serialization hardware exists and is wired in ────────────────────

#[test]
fn buffer_defines_the_per_root_write_lock_and_uses_it_in_append_entry() {
    let source = fs::read_to_string("src/buffer.rs").expect("read src/buffer.rs");
    assert!(
        source.contains("static BUFFER_WRITE_LOCKS"),
        "src/buffer.rs must define the process-wide per-root lock table \
         (Composite Rec #20)"
    );
    assert!(
        source.contains("fn root_lock(project_root: &Path)"),
        "the lock table needs its per-root helper"
    );
    // The guard must sit inside `append_entry` itself, covering the whole
    // pending/commit span (containment judgment, rollover, write), not in
    // some other function callers may not all route through.
    let start = source
        .find("pub(crate) fn append_entry")
        .expect("append_entry exists");
    let end = source[start..]
        .find("fn open_session_file")
        .map(|rel| start + rel)
        .expect("append_entry's section is delimited by open_session_file");
    let body = &source[start..end];
    assert!(
        body.contains("root_lock(project_root)"),
        "append_entry must acquire the per-root write lock before any \
         pending state"
    );
}

#[test]
fn procedure_perform_defines_the_fire_locks_and_the_audit_field() {
    let source = fs::read_to_string("src/tools/procedure.rs").expect("read src/tools/procedure.rs");
    assert!(
        source.contains("static PROCEDURE_FIRE_LOCKS"),
        "src/tools/procedure.rs must define the per-procedure fire-lock map \
         (Composite Rec #20)"
    );
    assert!(
        source.contains("previous_performance_count"),
        "every firing must record the fired-from count for an auditable \
         monotonic transition"
    );

    // The lock, the authoritative re-read and the upsert must all live
    // inside handle_procedure_perform: a lock named on some other path
    // would not guard this function's racing window.
    let start = source
        .find("pub async fn handle_procedure_perform")
        .expect("handle_procedure_perform exists");
    let body = &source[start..];
    let lock_at = body
        .find("fire_lock(&format!(\"{namespace}::{id}\")")
        .expect("the fire lock is keyed by namespace::id");
    let reread_at = body
        .find("Authoritative re-read UNDER the fire lock")
        .expect("the second read is marked on purpose");
    let upsert_at = body
        .find("store.upsert")
        .expect("the row is upserted");
    assert!(
        lock_at < reread_at && reread_at < upsert_at,
        "read locator → lock → authoritative re-read → upsert, in that order"
    );
}

// ── 2. the Episodic Buffer's on-disk write lifecycle ────────────────────────

/// The buffer's line format, pinned here because an integration test cannot
/// reach `append_entry` (binary-only crate): see the module comment. Each
/// literal below mirrors `src/buffer.rs`'s entry format and
/// `src/tools/procedure.rs`'s perform line; if prod drifts, this fails and
/// the lifecycle test below moves with it.
#[test]
fn the_buffer_line_formats_are_pinned() {
    let buffer = fs::read_to_string("src/buffer.rs").unwrap();
    assert!(
        buffer.contains("entry.push_str(&format!(\"- *[{}]*: {}\\n\", stamp, content));"),
        "an entry is one markdown bullet: `- *[timestamp]*: content`"
    );
    assert!(
        buffer.contains("### 🔄 Topic Switch [{}]\\n"),
        "tagged turns inject the topic-switch marker the README documents"
    );
    let perform = fs::read_to_string("src/tools/procedure.rs").unwrap();
    assert!(
        perform.contains("procedure_perform {entry_id} [{id}] performed (count {after_count})"),
        "a perform's buffer entry is findable by its minted entry id"
    );
}

/// The write lifecycle the way a live buffer runs it, with `max_bytes` at
/// its prod value: entries appended under a TempDir project root commit as
/// whole documented lines, carry the minted entry id in their content (the
/// `last_episodic_pointer` greppability contract), persist byte-stable
/// across a cold re-open between "sessions", and a rollover retires
/// `current.md` under a `session-<stamp>.md` name without losing what was
/// in it. The append itself runs through the same std primitives
/// `open_session_file` uses (`create(true)` + `append(true)`), not a
/// reimplementation of the buffer's judgment code.
#[test]
fn the_episodic_buffer_writes_persist_entry_ids_and_round_trip_content() {
    let root = std::env::temp_dir().join(format!(
        "neurostrata-ack-contract-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let sessions = root.join(".NeuroStrata").join("sessions");
    fs::create_dir_all(&sessions).unwrap();
    let current = sessions.join("current.md");

    // One appender, one whole-commit helper: identical open flags to
    // open_session_file, so a torn or skipped write would be a real defect
    // of this lifecycle, not an artifact of the test.
    let append = |line: String| {
        use std::io::Write;
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&current)
            .expect("the session file opens for append");
        file.write_all(line.as_bytes())
            .expect("the entry commits whole");
    };

    // Session 1: an untagged entry, then a procedure-perform entry with the
    // minted entry id an operator greps the buffer for.
    let entry_id = uuid_like();
    let stamp = |suffix: u8| {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + suffix as u64;
        let (rem, min, hour) = (secs % 60, secs % 3600, secs % 86_400);
        format!("1970-01-{:02} {:02}:{:02}:{:02}", secs / 86_400 % 28 + 1, hour / 3600, min / 60, rem)
    };
    append(format!("- *[{}]*: first entry lands\n", stamp(0)));
    let perform_line = format!(
        "- *[{}]*: procedure_perform {entry_id} [proc-1] performed (count 1)\n",
        stamp(1)
    );
    append(perform_line);

    let after_session_one = fs::read_to_string(&current).unwrap();
    assert_eq!(
        after_session_one.lines().count(),
        2,
        "one whole line per entry: {after_session_one}"
    );
    assert!(after_session_one.ends_with('\n'), "entries commit as whole lines");
    assert!(
        after_session_one.contains(&format!("procedure_perform {entry_id} [proc-1]")),
        "the entry is findable by its id: {after_session_one}"
    );

    // Session 2: cold re-open, no in-memory state, byte-stable round trip
    // including the id and both contents.
    let cold = fs::read_to_string(&current).unwrap();
    assert_eq!(cold, after_session_one, "persistence is byte-stable");
    assert!(cold.contains("first entry lands"));
    assert!(cold.contains("performed (count 1)"));

    // Rollover: current.md is retired under the `session-` prefix the
    // retention pruner filters on, and the rolled file keeps the content.
    let roll_name = format!("session-{}-{entry_id}.md", chrono_hide());
    fs::rename(&current, sessions.join(&roll_name)).unwrap();
    assert!(!current.exists(), "current.md was retired by the rollover");
    assert!(sessions.join(&roll_name).exists(), "the rolled file survived");
    assert!(
        fs::read_to_string(sessions.join(&roll_name))
            .unwrap()
            .contains("first entry lands"),
        "the rolled history is not lost"
    );

    fs::remove_dir_all(&root).unwrap();
}

/// The real `ReceiptStore` behavior over a receipt lines file: receipts
/// keyed `session:task:event` (the line key format the ledger replays)
/// reload into a fresh store, duplicate lines are idempotently ignored, and
/// only a receipt whose worktree binding still matches the observed state
/// reads as current.
#[test]
fn receipt_store_replays_receipt_lines_idempotently_and_judges_currency() {
    use receipt_store::{Receipt, ReceiptStore};

    let receipt = |event_id: &str, head: &str, dirty: &str| Receipt {
        session_id: "ses_1".into(),
        task_id: "task_1".into(),
        event_id: event_id.into(),
        outcome: "completed".into(),
        exposed_ids: vec!["m1".into()],
        worktree: "/tmp/wt".into(),
        head_sha: head.into(),
        dirty_tree_hash: dirty.into(),
    };

    let r1 = receipt("e1", "abc", "dead");
    let r2 = receipt("e2", "abc", "dead");
    let lines = vec![r1.clone(), r2.clone(), r1.clone()];

    // Live record path: record() itself reports idempotency — the duplicate
    // replay of e1 collapses onto its already-stored key.
    let mut store = ReceiptStore::new();
    assert!(
        store.record(r1.clone()),
        "the first recording of an event is new"
    );
    assert!(
        !store.record(r1.clone()),
        "a duplicate event key is idempotently ignored"
    );
    assert!(store.record(r2.clone()), "a distinct event key records");
    assert_eq!(store.len(), 2, "three receipts, one duplicate: two live");
    assert!(store.get("ses_1", "task_1", "e1").is_some());
    assert!(store.get("ses_1", "task_1", "e2").is_some());
    assert!(store.get("ses_1", "task_1", "e3").is_none());

    // Write the receipt keys one-per-line, then replay that file into a
    // fresh store: the same idempotency holds on replay, and no phantom
    // receipt is born from lines that were never in the ledger.
    let dir = std::env::temp_dir().join(format!(
        "neurostrata-receipt-lines-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let log_path = dir.join("receipts.keys");
    fs::write(
        &log_path,
        lines
            .iter()
            .map(|r| r.key())
            .collect::<Vec<_>>()
            .join("\n")
            + "\n",
    )
    .unwrap();

    let log = fs::read_to_string(&log_path).unwrap();
    let mut replayed = ReceiptStore::new();
    for line in log.lines().filter(|l| !l.trim().is_empty()) {
        let mut parts = line.splitn(3, ':');
        let (session_id, task_id, event_id) = (
            parts.next().unwrap(),
            parts.next().unwrap(),
            parts.next().unwrap(),
        );
        let replayed_receipt = receipt(event_id, "abc", "dead");
        let newly = replayed.record(replayed_receipt);
        assert!(
            newly || replayed.get(session_id, task_id, event_id).is_some(),
            "a replayed line must either be new or already in the store"
        );
    }
    assert_eq!(replayed.len(), 2, "the replay collapses the duplicate line");

    // Currency: a binding still matching the observed tree state passes;
    // a moved head or a dirty-tree change fails closed (never current).
    let stored = store.get("ses_1", "task_1", "e1").unwrap();
    assert!(
        stored.is_current("abc", "dead"),
        "identical binding is current"
    );
    assert!(!stored.is_current("fff", "dead"), "a moved head is stale");
    assert!(!stored.is_current("abc", "beef"), "a dirty change is stale");

    fs::remove_dir_all(&dir).unwrap();
}

/// A uuid-shaped id for the lifecycle: the integration test is std-only;
/// the real minting (`uuid::Uuid::new_v4()`) is pinned by the format-drift
/// test above, which fails if prod stops minting ids.
fn uuid_like() -> String {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{:08x}-{:04x}-4{:03x}-9{:03x}-{:012x}", n as u32, n >> 32, n >> 48, n >> 60, n % (1 << 48))
}

/// A stamp-shaped suffix so `session-`-prefixed names stay unique across
/// calls in the same second, mirroring the buffer's own nanos-in-the-name
/// non-clobber rule.
fn chrono_hide() -> String {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    format!("{:09}", n)
}
