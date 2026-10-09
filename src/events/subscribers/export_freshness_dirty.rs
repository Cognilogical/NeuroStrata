//! `ExportFreshnessDirty` -- the export-freshness gate, given a signal.
//!
//! The `export-freshness` wire has always answered one question -- is the
//! committed export behind the store? -- by probing file mtimes, because that
//! was all the store offered. This subscriber replaces the probe with a fact it
//! can read in one call: every storage mutation flips a per-namespace `dirty`
//! flag, and a future `export-graph` reads it and clears it.
//!
//! The flag is a sentinel row rather than a counter because the question is
//! "since the last export, did anything change?" -- a boolean survives a missed
//! flip, and a count cannot be reset without a separate write. It is one row
//! per namespace, in the namespace it describes, so two projects dirtying each
//! other is not possible.
//!
//! It is a row, not a file, because the gate and the `export-graph --clear`
//! run that serves it both read from the store; a flag on disk would need a
//! second path to the same answer and a way to stay consistent with it.

use crate::events::{
    MemorySubscriber, RecursionToken, SubscriberContext, SubscriberError, ThalamicPulse,
};
use crate::traits::{MemoryPayload, VectorStore};
use async_trait::async_trait;
use serde_json::json;

/// The `memory_type` that marks a row as the sentinel rather than a memory, so
/// the export-freshness gate can find it without guessing an id.
const FRESHNESS_FLAG_TYPE: &str = "freshness_flag";

/// The id prefix the sentinel carries. Namespaced because `upsert` MERGEs on id
/// alone, across every namespace in the store: two namespaces sharing one id
/// would make the second flip overwrite the first namespace's flag, and each
/// would silently stop reporting its own staleness.
const FRESHNESS_FLAG_ID_PREFIX: &str = "export_freshness";

/// The `user_id` a machine-written row carries. `system` is the same value the
/// rest of the codebase writes for rows no user authored.
const SYSTEM_USER_ID: &str = "system";

/// Marks a namespace's committed export as behind the store.
///
/// `dimensions` is the store's embedding width, because the sentinel is a row
/// like any other and `Memory.embedding` is a fixed-size `FLOAT[N]`. It is
/// taken at construction rather than read back per pulse: the width is a
/// property of the store the daemon opened, and asking it would cost a call on
/// the dispatcher's task for every pulse.
pub struct ExportFreshnessDirty {
    dimensions: usize,
}

impl ExportFreshnessDirty {
    /// `dimensions` is the same width the daemon built the store with, so the
    /// sentinel's zero vector is the width the engine will accept.
    pub fn new(dimensions: usize) -> Self {
        Self { dimensions }
    }
}

#[async_trait]
impl MemorySubscriber for ExportFreshnessDirty {
    fn name(&self) -> &'static str {
        "export_freshness_dirty"
    }

    async fn handle(
        &self,
        event: &ThalamicPulse,
        _token: &RecursionToken,
        ctx: &SubscriberContext<'_>,
    ) -> Result<(), SubscriberError> {
        // Every variant of the pulse is a storage mutation, and every storage
        // mutation means the committed export is behind. Matched by shape
        // rather than by arm so a new pulse kind inherits the flip instead of
        // silently going unrecorded. A guard verdict is not itself a mutation,
        // but `GuardEventLog` writes a row for it in this same round, so the
        // flag waits rather than claiming the export is clean.
        let reason = match event {
            ThalamicPulse::Created { .. } => "created",
            ThalamicPulse::Superseded { .. } => "superseded",
            ThalamicPulse::Archived { .. } => "archived",
            ThalamicPulse::GuardedActionFired { .. } => "guarded_action",
        };

        // The namespace comes off the context, which the dispatcher derived
        // from this same pulse -- so the row lands where the mutation was, not
        // where the subscriber was constructed.
        let namespace = ctx.namespace;
        ctx.store
            .upsert(namespace, &flag_id(namespace), vec![0.0; self.dimensions], dirty_payload(namespace, reason, ctx.ts))
            .await
    }
}

/// The sentinel's id for one namespace. Deterministic, so a second flip in the
/// same namespace MERGEs the same row rather than appending another.
fn flag_id(namespace: &str) -> String {
    format!("{FRESHNESS_FLAG_ID_PREFIX}::{namespace}")
}

/// The sentinel row: the flag, when it was last flipped, and what flipped it.
///
/// `dirty_reason` is for a human reading the row -- "superseded" says more than
/// a bare `true` -- and `source` is what `has_source` looks for, so the row is
/// auditable the same way a memory is.
fn dirty_payload(namespace: &str, reason: &str, ts: i64) -> MemoryPayload {
    MemoryPayload {
        content: format!("Committed export for {namespace} is behind the store ({reason})."),
        user_id: SYSTEM_USER_ID.to_string(),
        memory_type: FRESHNESS_FLAG_TYPE.to_string(),
        agent_name: Some("thalamic_bus".to_string()),
        location: String::new(),
        location_lines: String::new(),
        metadata: json!({
            "dirty": true,
            "dirty_at": ts,
            "dirty_reason": reason,
            "namespace": namespace,
            "source": "thalamic_bus:export_freshness_dirty",
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{RecursionToken, ThalamicBus, ThalamicPulse};
    use crate::store::LadybugStore;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    /// The store the flag row has to fit in. LadybugDB's `Memory.embedding` is
    /// a fixed-size `FLOAT[N]`, so this number is contract, not a fixture
    /// convenience: a subscriber that wrote the wrong length would have its
    /// write rejected by the engine.
    const TEST_DIMENSIONS: usize = 4;

    /// Long enough for the dispatcher to pop the queue and run every subscriber
    /// against the pulse, as the bus's own tests do.
    const DISPATCH_WAIT: Duration = Duration::from_millis(50);

    /// How long a test waits for a flag row to appear.
    ///
    /// Longer than the dispatcher's 250ms per-subscriber budget on purpose:
    /// the flag must land because the subscriber wrote it, not because a
    /// generous sleep papered over a slow write.
    const FLAG_WAIT: Duration = Duration::from_secs(5);

    /// A fresh store per test, in a directory named after the test, so a
    /// leftover from an earlier run can never be read back as this run's output.
    fn scratch(tag: &str) -> (PathBuf, LadybugStore) {
        let dir = std::env::temp_dir().join(format!(
            "neurostrata-freshness-dirty-test-{}-{}",
            std::process::id(),
            tag
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store =
            LadybugStore::for_testing(dir.join("freshness.lbug").to_string_lossy().to_string(), TEST_DIMENSIONS)
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
        bus.register(Box::new(ExportFreshnessDirty::new(TEST_DIMENSIONS)));
        (dir, bus, store)
    }

    async fn flag_of(store: &LadybugStore, namespace: &str) -> Option<MemoryPayload> {
        store.get(namespace, &flag_id(namespace)).await.ok().flatten().map(|(_, payload)| payload)
    }

    /// Waits for the flag to land. The bus dispatches on its own task, so the
    /// write finishes a moment after the pulse is emitted; polling is the
    /// honest way to wait for it rather than sleeping a guessed interval.
    async fn await_flag(store: &LadybugStore, namespace: &str) -> MemoryPayload {
        let deadline = Instant::now() + FLAG_WAIT;
        loop {
            if let Some(payload) = flag_of(store, namespace).await {
                return payload;
            }
            assert!(Instant::now() < deadline, "no freshness flag landed for {namespace:?} within {FLAG_WAIT:?}");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    fn created(namespace: &str) -> ThalamicPulse {
        ThalamicPulse::Created {
            id: format!("{namespace}-mem"),
            namespace: namespace.to_string(),
            kind: "task".to_string(),
        }
    }

    fn superseded(namespace: &str) -> ThalamicPulse {
        ThalamicPulse::Superseded {
            old_id: format!("{namespace}-old"),
            new_id: format!("{namespace}-new"),
            namespace: namespace.to_string(),
        }
    }

    fn archived(namespace: &str) -> ThalamicPulse {
        ThalamicPulse::Archived {
            id: format!("{namespace}-mem"),
            namespace: namespace.to_string(),
        }
    }

    #[tokio::test]
    async fn every_storage_mutation_marks_its_namespace_dirty() {
        let (_dir, bus, store) =
            wired("mutations", &["made", "revised", "retired"]).await;

        bus.emit(created("made"), &RecursionToken::root());
        bus.emit(superseded("revised"), &RecursionToken::root());
        bus.emit(archived("retired"), &RecursionToken::root());
        tokio::time::sleep(DISPATCH_WAIT).await;

        for (namespace, reason) in
            [("made", "created"), ("revised", "superseded"), ("retired", "archived")]
        {
            let payload = await_flag(&store, namespace).await;
            assert_eq!(payload.memory_type, FRESHNESS_FLAG_TYPE, "{namespace}: the row is the sentinel, not a memory");
            assert_eq!(payload.metadata["dirty"], serde_json::json!(true), "{namespace}: the flag is set");
            assert_eq!(
                payload.metadata["dirty_reason"],
                serde_json::json!(reason),
                "{namespace}: the row names the mutation that dirtied it"
            );
            assert_eq!(payload.metadata["namespace"], serde_json::json!(namespace), "{namespace}: the row names its own namespace");
            assert!(
                payload.metadata["dirty_at"].as_i64().expect("the flip is stamped") > 0,
                "{namespace}: the flip carries a timestamp"
            );
        }

        assert_eq!(bus.metrics().dispatcher_handled, 3, "every pulse reached the subscriber");
    }

    /// Step 5's drill, plus the concurrent pair. One namespace dirtied by a
    /// `Created` and another by an `Archived` must not bleed into each other,
    /// and a second pair of pulses landing in the same dispatch round must
    /// leave one row per namespace rather than one per pulse -- `upsert` MERGEs
    /// on id, so the second flip updates the first row's row instead of adding
    /// another.
    #[tokio::test]
    async fn the_flag_is_scoped_to_one_namespace_and_one_flip_stays_one_row() {
        let (_dir, bus, store) = wired("drill", &["a", "b"]).await;

        // The drill: `a` is dirtied by a Created, `b` by an Archived, and
        // neither pulse may say anything about the other namespace.
        bus.emit(created("a"), &RecursionToken::root());
        bus.emit(archived("b"), &RecursionToken::root());
        tokio::time::sleep(DISPATCH_WAIT).await;

        let a = await_flag(&store, "a").await;
        let b = await_flag(&store, "b").await;
        assert_eq!(a.metadata["dirty_reason"], serde_json::json!("created"), "a was dirtied by its own pulse");
        assert_eq!(b.metadata["dirty_reason"], serde_json::json!("archived"), "b was dirtied by its own pulse");
        assert_eq!(a.metadata["namespace"], serde_json::json!("a"));
        assert_eq!(b.metadata["namespace"], serde_json::json!("b"));

        // No cross-contamination: neither namespace holds a row naming the
        // other. `upsert` merges on id alone, not on (namespace, id), so two
        // namespaces sharing one id would leave one of these reads answering
        // with the other namespace's row.
        let ids_in_a: Vec<String> = store
            .list("a", None)
            .await
            .expect("a lists")
            .into_iter()
            .map(|r| r.id)
            .collect();
        assert_eq!(ids_in_a, vec![flag_id("a")], "namespace a holds its own flag and nothing else");

        // The concurrent pair: two pulses for different namespaces emitted back
        // to back, so the dispatcher runs them in the same round.
        bus.emit(superseded("a"), &RecursionToken::root());
        bus.emit(created("b"), &RecursionToken::root());
        tokio::time::sleep(DISPATCH_WAIT).await;

        let a = await_flag(&store, "a").await;
        let b = await_flag(&store, "b").await;
        assert_eq!(a.metadata["dirty_reason"], serde_json::json!("superseded"), "a took its own second flip");
        assert_eq!(b.metadata["dirty_reason"], serde_json::json!("created"), "b took its own second flip");

        let ids_in_a: Vec<String> =
            store.list("a", None).await.expect("a lists").into_iter().map(|r| r.id).collect();
        let ids_in_b: Vec<String> =
            store.list("b", None).await.expect("b lists").into_iter().map(|r| r.id).collect();
        assert_eq!(ids_in_a, vec![flag_id("a")], "two flips in one namespace are one row, updated");
        assert_eq!(ids_in_b, vec![flag_id("b")], "two flips in one namespace are one row, updated");

        assert_eq!(bus.metrics().dispatcher_handled, 4, "every pulse reached the subscriber");
    }

    /// The sentinel's embedding has to be the store's width. `Memory.embedding`
    /// is `FLOAT[N]`, so a zero vector of the wrong length is rejected at the
    /// write -- the flag would silently never land.
    #[tokio::test]
    async fn the_flag_row_round_trips_at_the_store_width() {
        let (_dir, bus, store) = wired("width", &["sized"]).await;

        bus.emit(created("sized"), &RecursionToken::root());
        tokio::time::sleep(DISPATCH_WAIT).await;
        await_flag(&store, "sized").await;

        let (vector, _payload) =
            store.get("sized", &flag_id("sized")).await.expect("a read").expect("the flag row");
        assert_eq!(vector.len(), TEST_DIMENSIONS, "the sentinel stores a zero vector of the store's width");
        assert!(vector.iter().all(|f| *f == 0.0), "and it is the zero vector: {vector:?}");
    }

    /// A guard verdict is not a storage mutation, but it is about to become
    /// one: `GuardEventLog` writes an audit row for it in the same dispatch
    /// round. Leaving the flag clean here would let `export-graph` report the
    /// namespace fresh and skip an audit row that is already in the store --
    /// a hole in the trail the flag exists to keep, and one that only opens on
    /// the quiet namespace where a guard is called rarely.
    #[tokio::test]
    async fn a_guard_verdict_also_marks_the_namespace_dirty() {
        let (_dir, bus, store) = wired("guarded", &["audited"]).await;

        bus.emit(
            ThalamicPulse::GuardedActionFired {
                trace_id: "guard-validate-4-4".to_string(),
                action_type: "shell".to_string(),
                payload_hash: 0xDEAD_BEEF,
                verdict: "deny".to_string(),
                rule_ids_triggered: vec!["rule.no-rm-rf".to_string()],
                namespace: "audited".to_string(),
            },
            &RecursionToken::root(),
        );
        tokio::time::sleep(DISPATCH_WAIT).await;

        let payload = await_flag(&store, "audited").await;
        assert_eq!(
            payload.metadata["dirty_reason"],
            serde_json::json!("guarded_action"),
            "the row names the guard call that dirtied the namespace"
        );
        assert_eq!(payload.metadata["dirty"], serde_json::json!(true));
        assert_eq!(bus.metrics().dispatcher_handled, 1, "the pulse reached the subscriber");
    }
}