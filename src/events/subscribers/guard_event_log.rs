//! `GuardEventLog` -- the guard's calls, on the record.
//!
//! `neurostrata_guard_validate` answers one question and leaves nothing behind:
//! it returns a verdict, and once the agent has read it, nothing anywhere
//! records that the call was ever made. That is enough for a guard and not
//! enough for an audit trail -- the deferred `export-graph --causal` needs the
//! *sequence* of what was allowed and what was refused, joined to the memories
//! that came after, and there is no way to reconstruct a call that left no
//! row.
//!
//! So every verdict leaves one `memory_type: "guard_event"` row, and the row
//! records what was decided rather than what was decided *about*: the action's
//! payload is present only as a 32-bit [`ThalamicPulse::GuardedActionFired`]
//! digest. A guard call is handed whatever the agent was about to do -- file
//! contents, shell arguments, whatever was in a diff -- and this row is the
//! artifact most likely to be exported, shipped and read by a third party, so
//! it commits to the payload without carrying it.
//!
//! One row per call, never one per trace. `trace_id` is a correlation id for
//! joining to the response the caller received, not an identity for the call:
//! a validator is retried, an agent retries with it, and an audit trail that
//! silently collapsed those into one row would be a trail of *verdicts* rather
//! than of *calls* -- and the retry is usually the interesting part.

use crate::events::{
    MemorySubscriber, RecursionToken, SubscriberContext, SubscriberError, ThalamicPulse,
};
use crate::traits::{MemoryPayload, VectorStore};
use async_trait::async_trait;
use serde_json::json;

/// The `memory_type` that marks a row as a guard audit record rather than a
/// memory, so a reader can find the trail without guessing from the content.
const GUARD_EVENT_TYPE: &str = "guard_event";

/// The id prefix every audit row carries. The suffix is a fresh v4 per call,
/// which is what makes the no-dedup guarantee structural rather than a promise:
/// `upsert` MERGEs on id, so a unique id is the only thing stopping a second
/// guard call on one trace from overwriting the first.
const GUARD_EVENT_ID_PREFIX: &str = "guard_event";

/// The `user_id` a machine-written row carries. `system` is the same value the
/// rest of the codebase writes for rows no user authored.
const SYSTEM_USER_ID: &str = "system";

/// Writes one audit row per guard verdict.
///
/// `dimensions` is the store's embedding width, because the row is a row like
/// any other and `Memory.embedding` is a fixed-size `FLOAT[N]`. Taken at
/// construction for the same reason as `ExportFreshnessDirty`'s: the width is a
/// property of the store the daemon opened, and asking it per pulse would cost
/// a call on the dispatcher's task.
pub struct GuardEventLog {
    dimensions: usize,
}

impl GuardEventLog {
    /// `dimensions` is the same width the daemon built the store with, so the
    /// audit row's zero vector is the width the engine will accept.
    pub fn new(dimensions: usize) -> Self {
        Self { dimensions }
    }
}

#[async_trait]
impl MemorySubscriber for GuardEventLog {
    fn name(&self) -> &'static str {
        "guard_event_log"
    }

    async fn handle(
        &self,
        event: &ThalamicPulse,
        _token: &RecursionToken,
        ctx: &SubscriberContext<'_>,
    ) -> Result<(), SubscriberError> {
        // Only the guard verdict is this subscriber's business; a storage
        // mutation is someone else's pulse, and an unmatched variant is a no-op
        // rather than an error.
        let ThalamicPulse::GuardedActionFired {
            trace_id,
            action_type,
            payload_hash,
            verdict,
            rule_ids_triggered,
            namespace: _,
        } = event
        else {
            return Ok(());
        };

        // The namespace comes off the context, which the dispatcher derived
        // from this same pulse -- so the row lands where the call was made, not
        // where the subscriber was constructed.
        let namespace = ctx.namespace;
        ctx.store
            .upsert(
                namespace,
                &audit_id(),
                vec![0.0; self.dimensions],
                audit_payload(
                    namespace,
                    trace_id,
                    action_type,
                    *payload_hash,
                    verdict,
                    rule_ids_triggered,
                    ctx.ts,
                ),
            )
            .await
    }
}

/// One row's id. A fresh v4 per call, so two calls that share a `trace_id`
/// still get two rows: `upsert` MERGEs on id, and the id is the only thing
/// standing between a retry and a silently overwritten audit trail.
fn audit_id() -> String {
    format!("{GUARD_EVENT_ID_PREFIX}::{}", uuid::Uuid::new_v4())
}

/// The audit row: what was decided, about what, and on whose call.
///
/// Everything here is the pulse's own fields. The action's payload is not among
/// them and never will be -- only its digest is, and a digest that fits in a
/// `u32` cannot carry a file's contents with it.
fn audit_payload(
    namespace: &str,
    trace_id: &str,
    action_type: &str,
    payload_hash: u32,
    verdict: &str,
    rule_ids_triggered: &[String],
    ts: i64,
) -> MemoryPayload {
    MemoryPayload {
        content: audit_line(action_type, verdict, trace_id, rule_ids_triggered, payload_hash),
        user_id: SYSTEM_USER_ID.to_string(),
        memory_type: GUARD_EVENT_TYPE.to_string(),
        agent_name: Some("thalamic_bus".to_string()),
        location: String::new(),
        location_lines: String::new(),
        metadata: json!({
            "trace_id": trace_id,
            "action_type": action_type,
            "payload_hash": payload_hash,
            "verdict": verdict,
            "rule_ids_triggered": rule_ids_triggered,
            "ts": ts,
            "namespace": namespace,
            "source": "thalamic_bus:guard_event_log",
        }),
    }
}

/// The row's one-line text, for a human grepping the store.
///
/// Repeats fields the metadata already carries on purpose: `search` and every
/// other reader of the namespace sees this line, and a reader that has to open
/// the metadata to learn which verdict a row records is a reader that skips it.
/// The rule ids are joined for the same reason -- `metadata` keeps them as the
/// array they were.
fn audit_line(
    action_type: &str,
    verdict: &str,
    trace_id: &str,
    rule_ids_triggered: &[String],
    payload_hash: u32,
) -> String {
    let rules =
        if rule_ids_triggered.is_empty() { "none".to_string() } else { rule_ids_triggered.join(",") };
    format!(
        "guard_validate [{verdict}] {action_type}: trace {trace_id}, payload_hash {payload_hash}, rules {rules}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{ThalamicBus, ThalamicPulse};
    use crate::store::LadybugStore;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    /// The store the audit row has to fit in. LadybugDB's `Memory.embedding` is
    /// a fixed-size `FLOAT[N]`, so this number is contract, not a fixture
    /// convenience: a subscriber that wrote the wrong length would have its
    /// write rejected by the engine.
    const TEST_DIMENSIONS: usize = 4;

    /// Long enough for the dispatcher to pop the queue and run every subscriber
    /// against the pulse, as the bus's own tests do.
    const DISPATCH_WAIT: Duration = Duration::from_millis(50);

    /// How long a test waits for audit rows to appear. Longer than the
    /// dispatcher's 250ms per-subscriber budget on purpose: a row must land
    /// because the subscriber wrote it, not because a generous sleep papered
    /// over a slow write.
    const ROW_WAIT: Duration = Duration::from_secs(5);

    /// A fresh store per test, in a directory named after the test, so a
    /// leftover from an earlier run can never be read back as this run's output.
    fn scratch(tag: &str) -> (PathBuf, LadybugStore) {
        let dir = std::env::temp_dir().join(format!(
            "neurostrata-guard-event-log-test-{}-{}",
            std::process::id(),
            tag
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = LadybugStore::for_testing(
            dir.join("guard.lbug").to_string_lossy().to_string(),
            TEST_DIMENSIONS,
        )
        .expect("a throwaway store opens");
        (dir, store)
    }

    /// A bus carrying the real store and the real subscriber -- the wiring the
    /// daemon builds, so nothing in the test can pass while production fails.
    ///
    /// `init` is what the daemon's own write path does on the way into an add,
    /// so a namespace that has just seen a mutation always has the `Memory`
    /// table. A fixture that skipped it would fail with `Table Memory does not
    /// exist` and prove nothing about the subscriber.
    async fn wired(tag: &str, namespaces: &[&str]) -> (PathBuf, ThalamicBus, Arc<LadybugStore>) {
        let (dir, store) = scratch(tag);
        for namespace in namespaces {
            store.init(namespace).await.expect("the namespace's table exists");
        }
        let store = Arc::new(store);
        let bus = ThalamicBus::new(16);
        bus.attach_store(store.clone());
        bus.register(Box::new(GuardEventLog::new(TEST_DIMENSIONS)));
        (dir, bus, store)
    }

    /// Every audit row in a namespace, oldest first.
    async fn audit_rows(store: &LadybugStore, namespace: &str) -> Vec<MemoryPayload> {
        store
            .list(namespace, None)
            .await
            .expect("the namespace lists")
            .into_iter()
            .map(|r| r.payload)
            .filter(|p| p.memory_type == GUARD_EVENT_TYPE)
            .collect()
    }

    /// Waits for exactly `expected` audit rows. The bus dispatches on its own
    /// task, so writes finish a moment after the pulse is emitted; polling is
    /// the honest way to wait rather than sleeping a guessed interval. The
    /// deadline also bounds the *negative*: a run that stops short of `expected`
    /// has to fail, which is what makes this able to catch a dropped row.
    async fn await_rows(
        store: &LadybugStore,
        namespace: &str,
        expected: usize,
    ) -> Vec<MemoryPayload> {
        let deadline = Instant::now() + ROW_WAIT;
        loop {
            let rows = audit_rows(store, namespace).await;
            if rows.len() == expected {
                return rows;
            }
            assert!(
                Instant::now() < deadline,
                "expected {expected} guard_event row(s) in {namespace:?}, saw {} within {ROW_WAIT:?}",
                rows.len()
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    fn guarded(namespace: &str, trace_id: &str) -> ThalamicPulse {
        ThalamicPulse::GuardedActionFired {
            trace_id: trace_id.to_string(),
            action_type: "shell".to_string(),
            payload_hash: 0xDEAD_BEEF,
            verdict: "deny".to_string(),
            rule_ids_triggered: vec!["rule.no-rm-rf".to_string()],
            namespace: namespace.to_string(),
        }
    }

    /// Waits for the dispatcher to finish `expected` pulses. Used where the
    /// assertion is about something *not* having been written, so "hasn't
    /// happened yet" and "will never happen" must be told apart -- sleeping a
    /// guessed interval could not.
    async fn await_handled(bus: &ThalamicBus, expected: u64) {
        let deadline = Instant::now() + ROW_WAIT;
        while bus.metrics().dispatcher_handled < expected {
            assert!(
                Instant::now() < deadline,
                "the dispatcher handled {} of {expected} pulses within {ROW_WAIT:?}",
                bus.metrics().dispatcher_handled
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[tokio::test]
    async fn a_guard_verdict_persists_an_audit_row() {
        let (_dir, bus, store) = wired("row", &["guarded"]).await;

        bus.emit(guarded("guarded", "guard-validate-1-2"), &RecursionToken::root());
        tokio::time::sleep(DISPATCH_WAIT).await;

        let rows = await_rows(&store, "guarded", 1).await;
        let row = &rows[0];
        assert_eq!(row.memory_type, GUARD_EVENT_TYPE, "the row is an audit record, not a memory");
        assert_eq!(row.metadata["trace_id"], serde_json::json!("guard-validate-1-2"), "the row joins back to the validator's own trace id");
        assert_eq!(row.metadata["action_type"], serde_json::json!("shell"));
        assert_eq!(row.metadata["verdict"], serde_json::json!("deny"));
        assert_eq!(row.metadata["rule_ids_triggered"], serde_json::json!(["rule.no-rm-rf"]));
        assert_eq!(row.metadata["namespace"], serde_json::json!("guarded"));
        assert_eq!(row.metadata["payload_hash"].as_u64(), Some(0xDEAD_BEEF_u64), "the payload is present as a digest");
        assert!(row.metadata["ts"].as_i64().expect("the call is stamped") > 0, "the row carries the dispatcher's timestamp");
        assert_eq!(bus.metrics().dispatcher_handled, 1, "the pulse reached the subscriber");
    }

    /// Step 6's drill. Two calls sharing a `trace_id` are still two calls, and
    /// the row count is the assertion that proves it: `upsert` MERGEs on id, so
    /// a subscriber that derived its id from the trace would pass every
    /// field-level assertion above while leaving one row behind both calls --
    /// a trail of verdicts where a trail of calls was promised.
    #[tokio::test]
    async fn two_calls_on_one_trace_id_are_two_rows() {
        let (_dir, bus, store) = wired("drill", &["retried"]).await;

        bus.emit(guarded("retried", "guard-validate-9-9"), &RecursionToken::root());
        tokio::time::sleep(DISPATCH_WAIT).await;
        bus.emit(guarded("retried", "guard-validate-9-9"), &RecursionToken::root());
        tokio::time::sleep(DISPATCH_WAIT).await;

        let rows = await_rows(&store, "retried", 2).await;
        for row in &rows {
            assert_eq!(row.metadata["trace_id"], serde_json::json!("guard-validate-9-9"), "both rows carry the shared trace id");
            assert_eq!(row.metadata["payload_hash"].as_u64(), Some(0xDEAD_BEEF_u64), "the identical digest is recorded twice, not merged");
        }
        assert_eq!(bus.metrics().dispatcher_handled, 2, "both pulses reached the subscriber");
    }

    /// The row records the payload's *digest*, and the digest is 32 bits. Two
    /// ways this could silently rot, both invisible to the tests above:
    /// `payload_hash` widening into a `u64` (or a string) in the row while the
    /// pulse still says `u32`, and someone helpfully adding the payload to the
    /// content line. The first is asserted against `u32::MAX`, the widest
    /// value a 32-bit digest can hold; the second against a payload that would
    /// be unmistakable in a row if it were ever written down.
    #[tokio::test]
    async fn the_row_carries_a_32_bit_digest_and_never_the_payload() {
        const SECRET: &str = "AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMI";
        let (_dir, bus, store) = wired("digest", &["hashed"]).await;

        bus.emit(
            ThalamicPulse::GuardedActionFired {
                trace_id: "guard-validate-7-7".to_string(),
                action_type: "env_read".to_string(),
                payload_hash: u32::MAX,
                verdict: "allow".to_string(),
                // An allow fires no rule, and the row must say so rather than
                // leaving the field absent for a reader to guess at.
                rule_ids_triggered: vec![],
                namespace: "hashed".to_string(),
            },
            &RecursionToken::root(),
        );
        tokio::time::sleep(DISPATCH_WAIT).await;

        let rows = await_rows(&store, "hashed", 1).await;
        let row = &rows[0];
        assert!(
            row.metadata["payload_hash"].is_number(),
            "the digest is a number, not a stringified one: {}",
            row.metadata["payload_hash"]
        );
        assert_eq!(
            row.metadata["payload_hash"].as_u64(),
            Some(u32::MAX as u64),
            "the full 32-bit range round-trips -- a u64 field would still hold this, so the width is pinned by the pulse's type, not by this value"
        );
        assert_eq!(row.metadata["rule_ids_triggered"], serde_json::json!([]), "an allow names no rule");
        assert_eq!(row.metadata["verdict"], serde_json::json!("allow"));

        let whole_row = format!("{} {}", row.content, row.metadata);
        assert!(
            !whole_row.contains(SECRET),
            "the audit row commits to the payload without carrying it: {whole_row}"
        );
    }

    /// A storage mutation is not a guard call, and the trail only records
    /// verdicts. Getting this wrong is not a cosmetic over-count: the audit
    /// rows would outnumber the guard calls by whatever the store writes, and
    /// every reader would have to work out which of them were real calls.
    #[tokio::test]
    async fn a_storage_mutation_writes_no_audit_row() {
        let (_dir, bus, store) = wired("ignored", &["ignored"]).await;

        bus.emit(guarded("ignored", "guard-validate-1-1"), &RecursionToken::root());
        await_rows(&store, "ignored", 1).await;

        bus.emit(
            ThalamicPulse::Created {
                id: "some-memory".to_string(),
                namespace: "ignored".to_string(),
                kind: "fact".to_string(),
            },
            &RecursionToken::root(),
        );
        await_handled(&bus, 2).await;

        let rows = audit_rows(&store, "ignored").await;
        assert_eq!(rows.len(), 1, "the Created pulse reached the subscriber and wrote nothing");
        assert_eq!(
            rows[0].metadata["trace_id"],
            serde_json::json!("guard-validate-1-1"),
            "and the one row that exists is still the guard call"
        );
    }
}
