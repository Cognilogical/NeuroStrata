//! The thalamic bus runtime: a bounded queue, one dispatcher task, and the
//! metrics a `POST /bus/metrics` consumer reads.
//!
//! `emit` never blocks the caller that just made the storage write: it hands
//! the pulse to a bounded queue (drop-oldest when full) and returns. A single
//! dispatcher task walks the queue and invokes every registered subscriber in
//! order, each under a 250ms timeout, each panic caught at the dispatcher join
//! point. Nothing a subscriber does can reach back into the caller's write.

use super::{
    BackpressureEvent, BusMetrics, MemorySubscriber, RecursionToken, SubscriberContext,
    ThalamicPulse,
};
use crate::traits::{MemoryPayload, RelocateOutcome, SearchResult, VectorStore};
use anyhow::anyhow;
use async_trait::async_trait;
use futures::FutureExt;
use std::any::Any;
use std::collections::VecDeque;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::sync::{watch, Notify};

/// Identifies a registered subscriber: its index in the registry. Stable for
/// the life of the bus (subscribers are never unregistered in v1).
pub type SubscriberId = usize;

/// Per-subscriber budget. Longer than any v1 subscriber needs; a runaway
/// subscriber is abandoned rather than allowed to stall the queue behind it.
const SUBSCRIBER_TIMEOUT: Duration = Duration::from_millis(250);

/// The six counters `BusMetrics` snapshots. Plain atomics: `emit` and the
/// dispatcher both bump them from different tasks, and none of them ever needs
/// a read-modify-write across events.
#[derive(Default)]
struct AtomicMetrics {
    events_emitted: AtomicU64,
    dispatcher_handled: AtomicU64,
    drops_oldest: AtomicU64,
    subscriber_panics: AtomicU64,
    subscriber_timeouts: AtomicU64,
    recursion_blocks: AtomicU64,
}

/// The daemon's store, once attached. Until then the dispatcher substitutes an
/// inert stand-in, because the dispatcher starts inside `new()` and the store
/// arrives later from `daemon.rs`.
///
/// A stand-in rather than a parked dispatch: every pulse still flows (metrics
/// stay truthful, subscribers that never touch `ctx.store` keep working), while
/// a subscriber that *does* read the store gets an explicit error naming the
/// missing attachment instead of a panic or a write to nowhere.
struct StoreNotAttached;

impl StoreNotAttached {
    fn err() -> anyhow::Error {
        anyhow!(
            "thalamic bus: no store attached -- call ThalamicBus::attach_store before the daemon serves requests"
        )
    }
}

#[async_trait]
impl VectorStore for StoreNotAttached {
    async fn init(&self, _namespace: &str) -> anyhow::Result<()> {
        Err(Self::err())
    }
    async fn upsert(
        &self,
        _namespace: &str,
        _id: &str,
        _vector: Vec<f32>,
        _payload: MemoryPayload,
    ) -> anyhow::Result<()> {
        Err(Self::err())
    }
    async fn search(
        &self,
        _namespace: &str,
        _vector: Vec<f32>,
        _limit: usize,
    ) -> anyhow::Result<Vec<SearchResult>> {
        Err(Self::err())
    }
    async fn delete(&self, _namespace: &str, _id: &str) -> anyhow::Result<()> {
        Err(Self::err())
    }
    async fn clear_ingested(&self, _namespace: &str) -> anyhow::Result<()> {
        Err(Self::err())
    }
    async fn relink_edges(&self, _namespace: &str) -> anyhow::Result<usize> {
        Err(Self::err())
    }
    async fn list(
        &self,
        _namespace: &str,
        _user_id: Option<&str>,
    ) -> anyhow::Result<Vec<SearchResult>> {
        Err(Self::err())
    }
    async fn get(
        &self,
        _namespace: &str,
        _id: &str,
    ) -> anyhow::Result<Option<(Vec<f32>, MemoryPayload)>> {
        Err(Self::err())
    }
    async fn relocate(&self, _id: &str, _from: &str, _to: &str) -> anyhow::Result<RelocateOutcome> {
        Err(Self::err())
    }
    async fn list_namespaces(&self) -> anyhow::Result<Vec<String>> {
        Err(Self::err())
    }
    async fn export_graph(&self, _include_retired: bool, _include_archived: bool) -> anyhow::Result<serde_json::Value> {
        Err(Self::err())
    }
    async fn increment_access_count(&self, _namespace: &str, _id: &str) -> anyhow::Result<()> {
        Err(Self::err())
    }
    async fn export_database(&self, _dir: &str) -> anyhow::Result<()> {
        Err(Self::err())
    }
    async fn checkpoint(&self) -> anyhow::Result<()> {
        Err(Self::err())
    }
}

/// The bounded pulse queue.
///
/// A `VecDeque` under a plain `std::sync::Mutex` rather than an mpsc channel,
/// because drop-oldest has to happen *inside the push that overflows it*. With
/// a channel whose receiver lives in the dispatcher's task, the pop that makes
/// room contends with a receiver that is parked holding its guard across the
/// `recv().await` whenever the queue is empty -- the common case. A producer
/// that cannot take the lock has nothing left to do but drop the *incoming*
/// pulse, silently turning drop-oldest into drop-newest. Here `emit` holds the
/// lock only for the pop+push it performs itself, never across an await, so
/// every queue-full decision is one atomic, uncontended-by-awaits critical
/// section.
///
/// `Notify` is the wakeup edge. `notify_one` stores a permit when the
/// dispatcher is not parked, so a push that lands between the dispatcher's
/// failed pop and its `notified().await` is never lost.
struct Queue {
    items: Mutex<VecDeque<ThalamicPulse>>,
    notify: Notify,
}

impl Queue {
    /// Push one pulse, trimming the oldest when the push puts the queue over
    /// `capacity`. Returns whether a pulse was evicted: at capacity 0 the
    /// pulse just pushed is also the oldest, and goes. The caller counts an
    /// eviction as a drop.
    fn push(&self, event: ThalamicPulse, capacity: usize) -> bool {
        let mut items = self.items.lock().expect("queue lock poisoned");
        items.push_back(event);
        let evicted = if items.len() > capacity {
            items.pop_front();
            true
        } else {
            false
        };
        // Release before waking: the waker only needs the edge, and it takes
        // the lock itself to pop.
        drop(items);
        self.notify.notify_one();
        evicted
    }

    fn pop(&self) -> Option<ThalamicPulse> {
        self.items.lock().expect("queue lock poisoned").pop_front()
    }

    fn len(&self) -> usize {
        self.items.lock().expect("queue lock poisoned").len()
    }
}

/// The bus. Explicit and owned: `daemon.rs` constructs one, attaches the
/// store, registers subscribers, and threads an `Arc<ThalamicBus>` to the
/// emit sites. Not a global, not `Clone` -- the `Arc` is the sharing.
pub struct ThalamicBus {
    queue: Arc<Queue>,
    subs: Arc<Mutex<Vec<Arc<dyn MemorySubscriber>>>>,
    metrics: Arc<AtomicMetrics>,
    bp_tx: watch::Sender<BackpressureEvent>,
    /// Stated capacity, kept so `emit` can trim without a second lookup.
    capacity: usize,
    store: Arc<OnceLock<Arc<dyn VectorStore>>>,
}

impl ThalamicBus {
    /// Construct the bus and start its dispatcher on the current Tokio
    /// runtime. Outside a runtime there is nothing to dispatch on: the bus is
    /// still constructible (`emit` is runtime-independent) but stays headless,
    /// with the failure logged rather than panicked -- emitted pulses queue
    /// and then drop oldest at capacity, exactly as they would with a
    /// dispatcher that died.
    pub fn new(capacity: usize) -> Self {
        // `Idle` is the seed: a consumer that reads before the first event
        // must not mistake a zero-count drop for real backpressure.
        let (bp_tx, _) = watch::channel(BackpressureEvent::Idle);
        let queue = Arc::new(Queue {
            items: Mutex::new(VecDeque::new()),
            notify: Notify::new(),
        });
        let subs: Arc<Mutex<Vec<Arc<dyn MemorySubscriber>>>> = Arc::new(Mutex::new(Vec::new()));
        let metrics = Arc::new(AtomicMetrics::default());
        let store = Arc::new(OnceLock::<Arc<dyn VectorStore>>::new());

        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(dispatch(
                    queue.clone(),
                    subs.clone(),
                    metrics.clone(),
                    bp_tx.clone(),
                    store.clone(),
                ));
            }
            Err(_) => tracing::warn!(
                "ThalamicBus constructed outside a Tokio runtime; no dispatcher started, emitted pulses will queue and then drop oldest at capacity"
            ),
        }

        Self { queue, subs, metrics, bp_tx, capacity, store }
    }

    /// Hand the daemon's store to the dispatcher, which stamps it into every
    /// `SubscriberContext`. The first store wins: a double attach cannot
    /// silently swap implementations under live subscribers.
    pub fn attach_store(&self, store: Arc<dyn VectorStore>) {
        if self.store.set(store).is_err() {
            tracing::warn!("ThalamicBus::attach_store called twice; keeping the first store");
        }
    }

    /// Register a subscriber. Returns its id for later introspection. The
    /// registry is append-only: ids are stable, and dispatch always sees the
    /// full list as of the pulse it is handling.
    pub fn register(&self, sub: Box<dyn MemorySubscriber>) -> SubscriberId {
        let mut subs = self.subs.lock().expect("subscriber registry lock poisoned");
        subs.push(Arc::from(sub));
        subs.len() - 1
    }

    /// Publish one pulse. Never blocks and never fails the caller's storage
    /// write. The recursion guard is checked here, at the bus boundary: a
    /// caller holding a token at depth ≥ 1 (a subscriber mid-invocation) is
    /// refused, the refusal is counted and published, and the pulse never
    /// reaches the queue. A full queue drops its oldest member to make room:
    /// the pulse that arrives is accepted, and whatever the push left behind
    /// capacity is what `drops_oldest` counts.
    pub fn emit(&self, event: ThalamicPulse, token: &RecursionToken) {
        if !token.allows_emit() {
            self.metrics.recursion_blocks.fetch_add(1, Ordering::Relaxed);
            let _ = self.bp_tx.send(BackpressureEvent::RecursionRefused {
                token_allows_emit: false,
            });
            tracing::debug!("pulse refused: caller holds a recursion token at depth >= 1");
            return;
        }
        let evicted = self.queue.push(event, self.capacity);
        self.metrics.events_emitted.fetch_add(1, Ordering::Relaxed);
        if evicted {
            self.note_drop();
        }
    }

    /// Account one lost pulse: counter, watch notification, and a WARN at the
    /// spec's 1-per-1000 rate so a drop storm cannot flood the log.
    fn note_drop(&self) {
        let drops = self.metrics.drops_oldest.fetch_add(1, Ordering::Relaxed) + 1;
        let _ = self.bp_tx.send(BackpressureEvent::Dropped { count: drops });
        if drops.is_multiple_of(1000) {
            tracing::warn!("pulse dropped: bus queue full -- {} drops total", drops);
        }
    }

    /// Subscribe to the sidecar stream of backpressure notifications.
    ///
    /// A fresh receiver reads [`BackpressureEvent::Idle`] until the bus
    /// publishes its first event.
    #[cfg(test)]
    pub fn backpressure(&self) -> watch::Receiver<BackpressureEvent> {
        self.bp_tx.subscribe()
    }

    /// A point-in-time snapshot of what the bus has done, for
    /// `POST /bus/metrics`. While the dispatcher is idle between pulses the
    /// snapshot satisfies `events_emitted == dispatcher_handled + queue_depth
    /// + drops_oldest`.
    pub fn metrics(&self) -> BusMetrics {
        BusMetrics {
            events_emitted: self.metrics.events_emitted.load(Ordering::Relaxed),
            dispatcher_handled: self.metrics.dispatcher_handled.load(Ordering::Relaxed),
            subscribers: self.subs.lock().expect("subscriber registry lock poisoned").len(),
            queue_depth: self.queue.len(),
            drops_oldest: self.metrics.drops_oldest.load(Ordering::Relaxed),
            subscriber_panics: self.metrics.subscriber_panics.load(Ordering::Relaxed),
            subscriber_timeouts: self.metrics.subscriber_timeouts.load(Ordering::Relaxed),
            recursion_blocks: self.metrics.recursion_blocks.load(Ordering::Relaxed),
        }
    }
}

/// Turn a panic payload into something a log line can carry. `catch_unwind`
/// hands back `Box<dyn Any>`; only `String`/`&str` payloads have text,
/// everything else is reported as such rather than dropped.
fn panic_text(payload: &(dyn Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else {
        "non-string panic payload".to_string()
    }
}

/// Cap panic text for the watch channel: first line only, control characters
/// stripped, 200 characters maximum (an ellipsis marks a cut). Panic text is
/// attacker- or content-influenced, and the watch channel is the path
/// `POST /bus/metrics` surfaces -- the same reasoning the spec gives
/// `GuardEventLog` for storing a payload *hash* instead of the payload. The
/// log line keeps the full text: logs are local, the watch channel is the
/// audit path.
fn redact_panic(text: &str) -> String {
    const MAX: usize = 200;
    // Panics often smuggle stack-trace context after the first line; the
    // watch channel never needs it.
    let first_line = text.lines().next().unwrap_or("");
    let cleaned: String = first_line.chars().filter(|c| !c.is_control()).collect();
    if cleaned.chars().count() <= MAX {
        cleaned
    } else {
        // The ellipsis is part of the cap, not a bonus: 199 chars + '…' = 200.
        let mut truncated: String = cleaned.chars().take(MAX - 1).collect();
        truncated.push('…');
        truncated
    }
}

/// Extract the namespace a pulse belongs to, so the context can name it without
/// carrying a second copy on the pulse.
fn namespace_of(event: &ThalamicPulse) -> &str {
    match event {
        ThalamicPulse::Created { namespace, .. } => namespace,
        ThalamicPulse::Superseded { namespace, .. } => namespace,
        ThalamicPulse::Archived { namespace, .. } => namespace,
        ThalamicPulse::GuardedActionFired { namespace, .. } => namespace,
    }
}

/// The single dispatcher task: pull one pulse, invoke every registered
/// subscriber against it, repeat. Single rather than task-per-subscriber for
/// ordering, and because backpressure is then one queue, not N.
///
/// The queue lock is taken for the pop only, never across a subscriber
/// invocation or an await, so it cannot interleave with `emit`'s critical
/// section. There is no channel-close to end the loop on: the daemon keeps
/// its `Arc<ThalamicBus>` alive for the process lifetime, and a runtime
/// shutdown takes the task with it.
async fn dispatch(
    queue: Arc<Queue>,
    subs: Arc<Mutex<Vec<Arc<dyn MemorySubscriber>>>>,
    metrics: Arc<AtomicMetrics>,
    bp_tx: watch::Sender<BackpressureEvent>,
    store: Arc<OnceLock<Arc<dyn VectorStore>>>,
) {
    loop {
        let event = match queue.pop() {
            Some(event) => event,
            None => {
                // Park until the next push. `notify_one`'s stored permit means
                // a push between the failed pop and this await is not missed.
                queue.notify.notified().await;
                continue;
            }
        };

        let namespace = namespace_of(&event);
        let ts = chrono::Utc::now().timestamp();
        let store: Arc<dyn VectorStore> =
            store.get().cloned().unwrap_or_else(|| Arc::new(StoreNotAttached));
        let ctx = SubscriberContext { store: store.as_ref(), namespace, ts };

        let registered: Vec<Arc<dyn MemorySubscriber>> =
            subs.lock().expect("subscriber registry lock poisoned").clone();
        for sub in registered.iter() {
            // A fresh depth-1 token per subscriber: the token exists to refuse
            // a subscriber emitting from inside its own invocation.
            let token = RecursionToken::new();
            // catch_unwind inside the timeout: a panicking subscriber is a
            // result to count, not an error to propagate -- the dispatcher
            // outlives every subscriber it runs.
            let outcome = tokio::time::timeout(
                SUBSCRIBER_TIMEOUT,
                AssertUnwindSafe(sub.handle(&event, &token, &ctx)).catch_unwind(),
            )
            .await;
            match outcome {
                Ok(Ok(Ok(()))) => {}
                Ok(Ok(Err(err))) => {
                    tracing::warn!(subscriber = sub.name(), "subscriber failed: {err:#}");
                }
                Ok(Err(payload)) => {
                    metrics.subscriber_panics.fetch_add(1, Ordering::Relaxed);
                    let text = panic_text(payload.as_ref());
                    // Redacted snippet onto the audit path; full text stays in
                    // the local log.
                    let snippet = redact_panic(&text);
                    let _ = bp_tx.send(BackpressureEvent::SubscriberPanic {
                        name: sub.name(),
                        payload: snippet,
                    });
                    tracing::warn!(subscriber = sub.name(), "subscriber panicked: {text}");
                }
                Err(_elapsed) => {
                    metrics.subscriber_timeouts.fetch_add(1, Ordering::Relaxed);
                    let _ = bp_tx.send(BackpressureEvent::SubscriberTimeout { name: sub.name() });
                    tracing::warn!(
                        subscriber = sub.name(),
                        "subscriber abandoned after {}ms timeout",
                        SUBSCRIBER_TIMEOUT.as_millis()
                    );
                }
            }
        }
        // The pulse is done: every registered subscriber got an invocation or
        // was counted by the panic/timeout arms above. This is the
        // `dispatcher_handled` term of the metrics invariant.
        metrics.dispatcher_handled.fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::SubscriberError;
    use std::sync::atomic::AtomicUsize;

    /// Counts `Created` pulses. The plainest possible proof that the
    /// dispatcher invoked a subscriber, and how many times.
    struct CountingSubscriber {
        counter: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl MemorySubscriber for CountingSubscriber {
        fn name(&self) -> &'static str {
            "counting"
        }

        async fn handle(
            &self,
            event: &ThalamicPulse,
            _token: &RecursionToken,
            _ctx: &SubscriberContext<'_>,
        ) -> Result<(), SubscriberError> {
            if let ThalamicPulse::Created { .. } = event {
                self.counter.fetch_add(1, Ordering::AcqRel);
            }
            Ok(())
        }
    }

    /// Holds the dispatcher busy for `delay` per pulse, so the bounded queue
    /// behind it actually fills. Records what it handled, proving dispatch
    /// continued after the drops -- and *which* pulses survived them.
    struct SlowSubscriber {
        log: Arc<Mutex<Vec<String>>>,
        delay: Duration,
    }

    #[async_trait]
    impl MemorySubscriber for SlowSubscriber {
        fn name(&self) -> &'static str {
            "slow"
        }

        async fn handle(
            &self,
            event: &ThalamicPulse,
            _token: &RecursionToken,
            _ctx: &SubscriberContext<'_>,
        ) -> Result<(), SubscriberError> {
            tokio::time::sleep(self.delay).await;
            let id = match event {
                ThalamicPulse::Archived { id, .. } => id.clone(),
                other => format!("{other:?}"),
            };
            self.log.lock().expect("slow log poisoned").push(id);
            Ok(())
        }
    }

    #[tokio::test]
    async fn emit_drops_oldest_when_queue_full() {
        let bus = ThalamicBus::new(2);
        let log = Arc::new(Mutex::new(Vec::new()));
        let log2 = log.clone();
        bus.register(Box::new(SlowSubscriber { log: log2, delay: Duration::from_millis(200) }));
        for i in 0..5 {
            bus.emit(
                ThalamicPulse::Archived { id: format!("{i}"), namespace: "n".into() },
                &RecursionToken::root(),
            );
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
        // Load-bearing: the surviving queue is the *newest* two pulses, so
        // the dispatcher must never have seen the three that were evicted.
        // Under drop-newest this log reads ["0", "1"], not ["3", "4"].
        assert_eq!(*log.lock().unwrap(), vec!["3".to_string(), "4".to_string()]);
        let m = bus.metrics();
        assert_eq!(m.events_emitted, 5);
        assert_eq!(m.drops_oldest, 3);
        assert_eq!(m.dispatcher_handled, 2);
        assert_eq!(m.queue_depth, 0);
        assert_eq!(
            m.events_emitted,
            m.dispatcher_handled + m.queue_depth as u64 + m.drops_oldest,
            "spec invariant: every accepted pulse was handled, queued, or dropped: {m:?}"
        );
    }

    /// Panics on every pulse, on purpose.
    struct PanicSubscriber;

    #[async_trait]
    impl MemorySubscriber for PanicSubscriber {
        fn name(&self) -> &'static str {
            "panicker"
        }

        async fn handle(
            &self,
            _event: &ThalamicPulse,
            _token: &RecursionToken,
            _ctx: &SubscriberContext<'_>,
        ) -> Result<(), SubscriberError> {
            panic!("subscriber exploded");
        }
    }

    /// Panics with caller-supplied text, to drill what reaches the watch
    /// channel when that text is hostile.
    struct TextPanicSubscriber {
        text: String,
    }

    #[async_trait]
    impl MemorySubscriber for TextPanicSubscriber {
        fn name(&self) -> &'static str {
            "text_panicker"
        }

        async fn handle(
            &self,
            _event: &ThalamicPulse,
            _token: &RecursionToken,
            _ctx: &SubscriberContext<'_>,
        ) -> Result<(), SubscriberError> {
            panic!("{}", self.text);
        }
    }

    /// The audit path must not carry raw panic content: control characters,
    /// stack-trace lines, and unbounded length are all trimmed before the
    /// pulse reaches the watch channel that `POST /bus/metrics` surfaces.
    #[tokio::test]
    async fn panic_payload_is_redacted_on_the_watch_channel() {
        let bus = ThalamicBus::new(16);
        let bp = bus.backpressure();
        bus.register(Box::new(TextPanicSubscriber {
            text: format!("boom\t\u{7}{}\nSECOND LINE of a stack trace", "y".repeat(1000)),
        }));
        bus.emit(
            ThalamicPulse::Created { id: "x".into(), namespace: "n".into(), kind: "task".into() },
            &RecursionToken::root(),
        );
        tokio::time::sleep(Duration::from_millis(50)).await;

        let received = bp.borrow().clone();
        let BackpressureEvent::SubscriberPanic { payload, .. } = &received else {
            panic!("expected SubscriberPanic on the watch channel, got {received:?}");
        };
        assert!(payload.starts_with("boom"), "the first line survives: {payload}");
        assert!(!payload.contains('\t') && !payload.contains('\u{7}'), "control chars stripped");
        assert!(!payload.contains('\n'), "only the first line may travel");
        assert!(!payload.contains("SECOND LINE"), "stack-trace context must not leak");
        assert!(
            payload.chars().count() <= 200,
            "payload capped at 200 chars including the ellipsis, got {}",
            payload.chars().count()
        );
        assert!(payload.ends_with('…'), "a truncated payload says so");
        assert_eq!(bus.metrics().subscriber_panics, 1);
    }

    /// Same behavior as [`CountingSubscriber`]; a distinct name so the
    /// second-subscriber assertions read as "the one after the panicker".
    type CountingSubscriber2 = CountingSubscriber;

    #[tokio::test]
    async fn subscriber_panic_is_caught() {
        let bus = ThalamicBus::new(16);
        let after = Arc::new(AtomicUsize::new(0));
        let after2 = after.clone();
        bus.register(Box::new(PanicSubscriber));
        bus.register(Box::new(CountingSubscriber2 { counter: after2 }));
        bus.emit(
            ThalamicPulse::Created { id: "x".into(), namespace: "n".into(), kind: "task".into() },
            &RecursionToken::root(),
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            after.load(Ordering::Acquire),
            1,
            "second subscriber still ran after first panicked"
        );
        let m = bus.metrics();
        assert_eq!(m.subscriber_panics, 1);
    }

    /// The failure path of the recursion guard, drilled: a depth-1 token (the
    /// one the dispatcher hands subscribers -- NOT a `root()`) must make the
    /// bus drop the pulse at the boundary, count the block, and publish the
    /// refusal. Proven by a subscriber that must never fire.
    #[tokio::test]
    async fn recursion_blocked_under_held_token() {
        let bus = ThalamicBus::new(16);
        let ran = Arc::new(AtomicUsize::new(0));
        let ran2 = ran.clone();
        bus.register(Box::new(CountingSubscriber { counter: ran2 }));
        let bp = bus.backpressure();

        bus.emit(
            ThalamicPulse::Created { id: "x".into(), namespace: "n".into(), kind: "task".into() },
            &RecursionToken::new(),
        );
        tokio::time::sleep(Duration::from_millis(50)).await;

        let m = bus.metrics();
        assert_eq!(m.recursion_blocks, 1, "blocked emit must count a recursion block: {:?}", m);
        assert_eq!(m.events_emitted, 0, "blocked emit must not enter the queue: {:?}", m);
        assert_eq!(ran.load(Ordering::Acquire), 0, "no subscriber may see a blocked pulse");
        assert_eq!(
            *bp.borrow(),
            BackpressureEvent::RecursionRefused { token_allows_emit: false },
            "the refusal must be distinguishable from a queue-full drop"
        );
    }

    /// Sleeps through its own timeout, on purpose: the dispatcher must abandon
    /// it at 250ms, count the timeout, publish it, and keep the queue moving
    /// to the next subscriber.
    struct Sleeper {
        delay: Duration,
    }

    #[async_trait]
    impl MemorySubscriber for Sleeper {
        fn name(&self) -> &'static str {
            "sleeper"
        }

        async fn handle(
            &self,
            _event: &ThalamicPulse,
            _token: &RecursionToken,
            _ctx: &SubscriberContext<'_>,
        ) -> Result<(), SubscriberError> {
            tokio::time::sleep(self.delay).await;
            Ok(())
        }
    }

    #[tokio::test]
    async fn subscriber_timeout_abandons_slow_subscriber() {
        let bus = ThalamicBus::new(16);
        let ran = Arc::new(AtomicUsize::new(0));
        let ran2 = ran.clone();
        bus.register(Box::new(Sleeper { delay: Duration::from_millis(500) }));
        bus.register(Box::new(CountingSubscriber { counter: ran2 }));
        let bp = bus.backpressure();

        bus.emit(
            ThalamicPulse::Created { id: "x".into(), namespace: "n".into(), kind: "task".into() },
            &RecursionToken::root(),
        );
        // Past the 250ms budget, before the sleeper's own 500ms would end.
        tokio::time::sleep(Duration::from_millis(400)).await;

        let m = bus.metrics();
        assert_eq!(m.subscriber_timeouts, 1, "slow subscriber must be abandoned once: {:?}", m);
        assert_eq!(
            ran.load(Ordering::Acquire),
            1,
            "dispatcher must reach the next subscriber after abandoning one"
        );
        assert_eq!(
            *bp.borrow(),
            BackpressureEvent::SubscriberTimeout { name: "sleeper" },
            "the timeout must be published on the watch channel"
        );
    }

    #[tokio::test]
    async fn dispatcher_invokes_registered_subscriber() {
        let bus = ThalamicBus::new(16);
        let rec = Arc::new(AtomicUsize::new(0));
        let rec2 = rec.clone();
        bus.register(Box::new(CountingSubscriber { counter: rec2 }));
        bus.emit(
            ThalamicPulse::Created { id: "x".into(), namespace: "n".into(), kind: "task".into() },
            &RecursionToken::root(),
        );
        // give dispatcher time
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(rec.load(Ordering::Acquire), 1, "subscriber ran exactly once");
    }

    /// Minor #9: the watch channel's seed must say "nothing happened", not
    /// "the queue was full zero times".
    #[tokio::test]
    async fn backpressure_starts_idle() {
        let bus = ThalamicBus::new(16);
        assert_eq!(
            *bus.backpressure().borrow(),
            BackpressureEvent::Idle,
            "a fresh receiver must read Idle, never a zero-count Dropped"
        );
    }

    /// A store that records how many times `get` reached it and answers
    /// successfully. Enough to tell an attached store apart from the inert
    /// stand-in (which always errors) by behavior, not just pointer identity.
    struct RecordingStore {
        gets: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl VectorStore for RecordingStore {
        async fn init(&self, _namespace: &str) -> anyhow::Result<()> {
            Err(anyhow!("recording store: unused by this test"))
        }
        async fn upsert(
            &self,
            _namespace: &str,
            _id: &str,
            _vector: Vec<f32>,
            _payload: MemoryPayload,
        ) -> anyhow::Result<()> {
            Err(anyhow!("recording store: unused by this test"))
        }
        async fn search(
            &self,
            _namespace: &str,
            _vector: Vec<f32>,
            _limit: usize,
        ) -> anyhow::Result<Vec<SearchResult>> {
            Err(anyhow!("recording store: unused by this test"))
        }
        async fn delete(&self, _namespace: &str, _id: &str) -> anyhow::Result<()> {
            Err(anyhow!("recording store: unused by this test"))
        }
        async fn clear_ingested(&self, _namespace: &str) -> anyhow::Result<()> {
            Err(anyhow!("recording store: unused by this test"))
        }
        async fn relink_edges(&self, _namespace: &str) -> anyhow::Result<usize> {
            Err(anyhow!("recording store: unused by this test"))
        }
        async fn list(
            &self,
            _namespace: &str,
            _user_id: Option<&str>,
        ) -> anyhow::Result<Vec<SearchResult>> {
            Err(anyhow!("recording store: unused by this test"))
        }
        async fn get(
            &self,
            _namespace: &str,
            _id: &str,
        ) -> anyhow::Result<Option<(Vec<f32>, MemoryPayload)>> {
            self.gets.fetch_add(1, Ordering::AcqRel);
            Ok(None)
        }
        async fn relocate(
            &self,
            _id: &str,
            _from: &str,
            _to: &str,
        ) -> anyhow::Result<RelocateOutcome> {
            Err(anyhow!("recording store: unused by this test"))
        }
        async fn list_namespaces(&self) -> anyhow::Result<Vec<String>> {
            Err(anyhow!("recording store: unused by this test"))
        }
        async fn export_graph(&self, _include_retired: bool, _include_archived: bool) -> anyhow::Result<serde_json::Value> {
            Err(anyhow!("recording store: unused by this test"))
        }
        async fn increment_access_count(&self, _namespace: &str, _id: &str) -> anyhow::Result<()> {
            Err(anyhow!("recording store: unused by this test"))
        }
        async fn export_database(&self, _dir: &str) -> anyhow::Result<()> {
            Err(anyhow!("recording store: unused by this test"))
        }
        async fn checkpoint(&self) -> anyhow::Result<()> {
            Err(anyhow!("recording store: unused by this test"))
        }
    }

    /// Calls `ctx.store.get` once per pulse and records the outcome: the
    /// observable difference between an attached store and the stand-in.
    struct StoreProbingSubscriber {
        oks: Arc<AtomicUsize>,
        errs: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl MemorySubscriber for StoreProbingSubscriber {
        fn name(&self) -> &'static str {
            "store_probe"
        }

        async fn handle(
            &self,
            _event: &ThalamicPulse,
            _token: &RecursionToken,
            ctx: &SubscriberContext<'_>,
        ) -> Result<(), SubscriberError> {
            match ctx.store.get(ctx.namespace, "probe").await {
                Ok(_) => {
                    self.oks.fetch_add(1, Ordering::AcqRel);
                }
                Err(_) => {
                    self.errs.fetch_add(1, Ordering::AcqRel);
                }
            }
            Ok(())
        }
    }

    /// The contract Task 3 depends on: `attach_store` before the first pulse
    /// lands in `ctx.store`, verbatim -- the subscriber's call reaches *the
    /// attached instance*, proven by its call counter.
    #[tokio::test]
    async fn attach_store_reaches_subscriber_context() {
        let bus = ThalamicBus::new(16);
        let gets = Arc::new(AtomicUsize::new(0));
        bus.attach_store(Arc::new(RecordingStore { gets: gets.clone() }));
        let oks = Arc::new(AtomicUsize::new(0));
        let errs = Arc::new(AtomicUsize::new(0));
        bus.register(Box::new(StoreProbingSubscriber { oks: oks.clone(), errs: errs.clone() }));
        bus.emit(
            ThalamicPulse::Created { id: "x".into(), namespace: "n".into(), kind: "task".into() },
            &RecursionToken::root(),
        );
        tokio::time::sleep(Duration::from_millis(50)).await;

        assert_eq!(oks.load(Ordering::Acquire), 1, "the attached store must answer the probe");
        assert_eq!(errs.load(Ordering::Acquire), 0, "the stand-in must not be in the context");
        assert_eq!(gets.load(Ordering::Acquire), 1, "the call reached the attached instance");
    }

    /// A double attach must not swap implementations under live subscribers:
    /// the first store is the one that keeps receiving calls. (The
    /// `tracing::warn!` the second attach emits is not captured here: log
    /// capture needs a tracing-subscriber dev dependency, which v1 forbids;
    /// the keep-first contract is what the counters prove.)
    #[tokio::test]
    async fn attach_store_twice_keeps_first() {
        let bus = ThalamicBus::new(16);
        let first = Arc::new(AtomicUsize::new(0));
        let second = Arc::new(AtomicUsize::new(0));
        bus.attach_store(Arc::new(RecordingStore { gets: first.clone() }));
        bus.attach_store(Arc::new(RecordingStore { gets: second.clone() }));
        let oks = Arc::new(AtomicUsize::new(0));
        let errs = Arc::new(AtomicUsize::new(0));
        bus.register(Box::new(StoreProbingSubscriber { oks: oks.clone(), errs: errs.clone() }));
        bus.emit(
            ThalamicPulse::Created { id: "x".into(), namespace: "n".into(), kind: "task".into() },
            &RecursionToken::root(),
        );
        tokio::time::sleep(Duration::from_millis(50)).await;

        assert_eq!(first.load(Ordering::Acquire), 1, "the first attach is the store in ctx.store");
        assert_eq!(second.load(Ordering::Acquire), 0, "the second attach must be a no-op");
        assert_eq!(oks.load(Ordering::Acquire), 1);
    }

    /// Before `attach_store`, pulses still flow and `ctx.store` is the
    /// stand-in: operations fail with the documented error rather than a
    /// panic or a silent write to nowhere.
    #[tokio::test]
    async fn before_attach_subscriber_store_errors() {
        let bus = ThalamicBus::new(16);
        let oks = Arc::new(AtomicUsize::new(0));
        let errs = Arc::new(AtomicUsize::new(0));
        bus.register(Box::new(StoreProbingSubscriber { oks: oks.clone(), errs: errs.clone() }));
        bus.emit(
            ThalamicPulse::Created { id: "x".into(), namespace: "n".into(), kind: "task".into() },
            &RecursionToken::root(),
        );
        tokio::time::sleep(Duration::from_millis(50)).await;

        assert_eq!(errs.load(Ordering::Acquire), 1, "the stand-in must answer explicitly");
        assert_eq!(oks.load(Ordering::Acquire), 0, "nothing may read through an unattached store");
        // The pulse itself still flowed -- metrics stay truthful.
        let m = bus.metrics();
        assert_eq!(m.events_emitted, 1);
        assert_eq!(m.dispatcher_handled, 1);
    }

    /// The spec's degenerate bus: capacity 0 accepts nothing -- every pulse
    /// is counted as emitted and immediately dropped, and the queue stays
    /// empty.
    #[tokio::test]
    async fn zero_capacity_bus_drops_every_pulse() {
        let bus = ThalamicBus::new(0);
        let bp = bus.backpressure();
        bus.emit(
            ThalamicPulse::Created { id: "x".into(), namespace: "n".into(), kind: "task".into() },
            &RecursionToken::root(),
        );
        let m = bus.metrics();
        assert_eq!(m.events_emitted, 1);
        assert_eq!(m.drops_oldest, 1);
        assert_eq!(m.queue_depth, 0);
        assert_eq!(*bp.borrow(), BackpressureEvent::Dropped { count: 1 });
    }
}
