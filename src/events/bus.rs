//! The thalamic bus runtime: a bounded queue, one dispatcher task, and the
//! metrics a `POST /bus/metrics` consumer reads.
//!
//! `emit` never blocks the caller that just made the storage write: it hands
//! the pulse to a bounded queue (drop-oldest when full) and returns. A single
//! dispatcher task walks the queue and invokes every registered subscriber in
//! order, each under a 250ms timeout, each panic caught at the dispatcher join
//! point. Nothing a subscriber does can reach back into the caller's write.

use super::outbox::{EventClass, EventOutbox, OutboxError};
use super::{
    BackpressureEvent, BusMetrics, MemorySubscriber, RecursionToken, SubscriberContext,
    ThalamicPulse,
};
use crate::traits::{MemoryPayload, RelocateOutcome, SearchResult, VectorStore};
use crate::util::lock_or_err;
use anyhow::anyhow;
use async_trait::async_trait;
use futures::FutureExt;
use std::any::Any;
use std::collections::VecDeque;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
    outbox_append_failures: AtomicU64,
    outbox_attached: AtomicBool,
    outbox_attach_failed: AtomicBool,
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
    ) -> anyhow::Result<crate::traits::UpsertReport> {
        Err(Self::err())
    }
    async fn search(
        &self,
        _namespace: &str,
        _vector: Vec<f32>,
        _limit: usize,
        _include_tasks: bool,
        _include_archived: bool,
    ) -> anyhow::Result<Vec<SearchResult>> {
        Err(Self::err())
    }
    async fn delete(&self, _namespace: &str, _id: &str) -> anyhow::Result<()> {
        Err(Self::err())
    }
    async fn clear_ingested(&self, _namespace: &str) -> anyhow::Result<()> {
        Err(Self::err())
    }
    async fn relink_edges(&self, _namespace: &str, _source_key: &str) -> anyhow::Result<usize> {
        Err(Self::err())
    }
    async fn list(
        &self,
        _namespace: &str,
        _user_id: Option<&str>,
        _memory_type: Option<&str>,
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
    async fn export_graph(
        &self,
        _include_retired: bool,
        _include_archived: bool,
        _project_contexts: &std::collections::HashMap<String, std::path::PathBuf>,
        _anchor_override: Option<&crate::source_context::ScopedAnchorOverride>,
    ) -> anyhow::Result<serde_json::Value> {
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

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl crate::traits::AsAny for StoreNotAttached {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// One queued pulse plus the durable classes whose outbox append
/// succeeded before it was queued. The dispatcher may suppress queue
/// delivery to a class's own subscriber only for a class named here: a
/// failed append has no outbox entry for a worker to deliver, so that
/// pulse must retain its ordinary queue path.
struct QueuedPulse {
    event: ThalamicPulse,
    appended_classes: Vec<EventClass>,
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
    items: Mutex<VecDeque<QueuedPulse>>,
    notify: Notify,
}

impl Queue {
    /// Push one pulse, trimming the oldest when the push puts the queue over
    /// `capacity`. Returns whether a pulse was evicted: at capacity 0 the
    /// pulse just pushed is also the oldest, and goes. The caller counts an
    /// eviction as a drop.
    fn push(
        &self,
        event: ThalamicPulse,
        appended_classes: Vec<EventClass>,
        capacity: usize,
    ) -> bool {
        // Poison disposition: the pulse cannot be queued, so it is
        // reported as dropped (the caller's drop accounting stays honest)
        // and the failure logged, never a panic.
        let mut items = match lock_or_err(&self.items) {
            Ok(items) => items,
            Err(e) => {
                tracing::error!(error = %e, "bus queue lock poisoned; pulse dropped");
                return true;
            }
        };
        items.push_back(QueuedPulse {
            event,
            appended_classes,
        });
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

    fn pop(&self) -> Option<QueuedPulse> {
        // Poison disposition: pop reports empty (the dispatcher parks and
        // retries on the next push), with the failure logged.
        match lock_or_err(&self.items) {
            Ok(mut items) => items.pop_front(),
            Err(e) => {
                tracing::error!(error = %e, "bus queue lock poisoned; pop reports empty");
                None
            }
        }
    }

    fn len(&self) -> usize {
        // Poison disposition: the snapshot reports depth 0, logged.
        match lock_or_err(&self.items) {
            Ok(items) => items.len(),
            Err(e) => {
                tracing::error!(error = %e, "bus queue lock poisoned; depth reports 0");
                0
            }
        }
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
    /// The durable event outbox (Task 12), once the daemon attaches one.
    /// `None` is the bus every test and non-daemon consumer runs — bounded
    /// queue semantics exactly as before. `Some` switches
    /// durability-qualified pulses to the durable path: a synchronous
    /// append inside `emit`, and delivery by the per-class workers rather
    /// than by the dispatcher.
    outbox: Arc<Mutex<Option<Arc<EventOutbox>>>>,
    /// The wakeup edge for the delivery workers: an append notifies, and
    /// each worker re-checks its own class's pending list.
    outbox_notify: Arc<Notify>,
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
        let outbox: Arc<Mutex<Option<Arc<EventOutbox>>>> = Arc::new(Mutex::new(None));
        let outbox_notify = Arc::new(Notify::new());

        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(dispatch(
                    queue.clone(),
                    subs.clone(),
                    metrics.clone(),
                    bp_tx.clone(),
                    store.clone(),
                    outbox.clone(),
                ));
            }
            Err(_) => tracing::warn!(
                "ThalamicBus constructed outside a Tokio runtime; no dispatcher started, emitted pulses will queue and then drop oldest at capacity"
            ),
        }

        Self {
            queue,
            subs,
            metrics,
            bp_tx,
            capacity,
            store,
            outbox,
            outbox_notify,
        }
    }

    /// Attach the durable event outbox (Task 12) over `dir` and start the
    /// per-class delivery workers. The daemon calls this once, before it
    /// serves: the workers' first work is replaying whatever a previous
    /// run left unacknowledged, and because every qualified pulse is
    /// delivered by its class worker in sequence order, a replayed entry
    /// stays ahead of any live pulse appended after it.
    ///
    /// Fail-soft, like the mesh wiring: if the outbox cannot open, the
    /// error is logged and the bus stays on the bounded queue — a daemon
    /// that cannot make its audit durable must say so loudly, not refuse
    /// to start. The first attach wins; a second is a logged no-op.
    pub fn attach_event_outbox(&self, dir: impl Into<PathBuf>) {
        let dir = dir.into();
        if self.outbox().is_some() {
            self.metrics
                .outbox_attached
                .store(true, Ordering::Relaxed);
            self.metrics
                .outbox_attach_failed
                .store(false, Ordering::Relaxed);
            tracing::warn!(
                "ThalamicBus::attach_event_outbox called twice; keeping the first outbox"
            );
            return;
        }
        let outbox = match EventOutbox::open(&dir) {
            Ok(outbox) => Arc::new(outbox),
            Err(err) => {
                self.metrics
                    .outbox_attached
                    .store(false, Ordering::Relaxed);
                self.metrics
                    .outbox_attach_failed
                    .store(true, Ordering::Relaxed);
                tracing::error!(
                    "event outbox could not open at {}: {err}; audit and mesh events stay on the lossy bounded queue",
                    dir.display()
                );
                return;
            }
        };
        {
            let mut slot = self.outbox.lock().expect("outbox slot lock poisoned");
            if slot.is_some() {
                self.metrics
                    .outbox_attached
                    .store(true, Ordering::Relaxed);
                self.metrics
                    .outbox_attach_failed
                    .store(false, Ordering::Relaxed);
                tracing::warn!(
                    "ThalamicBus::attach_event_outbox called twice; keeping the first outbox"
                );
                return;
            }
            *slot = Some(outbox.clone());
        }
        self.metrics
            .outbox_attached
            .store(true, Ordering::Relaxed);
        self.metrics
            .outbox_attach_failed
            .store(false, Ordering::Relaxed);
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                for class in EventClass::all() {
                    handle.spawn(delivery_worker(
                        class,
                        outbox.clone(),
                        self.subs.clone(),
                        self.store.clone(),
                        self.metrics.clone(),
                        self.bp_tx.clone(),
                        self.outbox_notify.clone(),
                    ));
                }
            }
            Err(_) => tracing::warn!(
                "event outbox attached outside a Tokio runtime; delivery workers not started, entries accumulate for the next start"
            ),
        }
    }

    /// The attached outbox, if any.
    fn outbox(&self) -> Option<Arc<EventOutbox>> {
        self.outbox
            .lock()
            .expect("outbox slot lock poisoned")
            .clone()
    }

    /// The classes `event` is durability-qualified for *right now*: a
    /// class qualifies iff an outbox is attached, its named subscriber is
    /// registered, and that class is interested in the pulse. Follows the
    /// subscription registry — never a hardcoded list of event kinds.
    fn qualified_classes(&self, event: &ThalamicPulse) -> Vec<EventClass> {
        if self.outbox().is_none() {
            return Vec::new();
        }
        let subs = self.subs.lock().expect("subscriber registry lock poisoned");
        EventClass::all()
            .into_iter()
            .filter(|class| {
                class.interested_in(event)
                    && subs
                        .iter()
                        .any(|s| EventClass::class_of_subscriber(s.name()) == Some(*class))
            })
            .collect()
    }

    /// Durably append `event` for every class it qualifies for, inside
    /// `emit` and before queue dispatch — the append is a file write, and
    /// synchronous is the honest shape at this seam. Every failure is
    /// counted in `outbox_append_failures` and logged at `error` before
    /// this returns: the failure counter is the record, and there is no
    /// silent path. The returned classes are exactly those whose append
    /// succeeded, so the queued pulse can tell the dispatcher which class
    /// subscribers the delivery workers now own; a class absent from the
    /// list retains ordinary queue delivery as its fallback.
    fn append_qualified(
        &self,
        event: &ThalamicPulse,
    ) -> (Vec<EventClass>, Option<OutboxError>) {
        let classes = self.qualified_classes(event);
        if classes.is_empty() {
            return (Vec::new(), None);
        }
        let outbox = self.outbox().expect("qualified implies an attached outbox");
        let mut appended_classes = Vec::new();
        let mut first_err = None;
        for class in classes {
            match outbox.append(class, event) {
                Ok(_) => {
                    appended_classes.push(class);
                    self.outbox_notify.notify_waiters();
                }
                Err(err) => {
                    self.metrics
                        .outbox_append_failures
                        .fetch_add(1, Ordering::Relaxed);
                    tracing::error!(
                        class = class.as_str(),
                        "durable outbox append failed: {err}"
                    );
                    if first_err.is_none() {
                        first_err = Some(err);
                    }
                }
            }
        }
        (appended_classes, first_err)
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
    ///
    /// Poison disposition: if the registry lock is poisoned the
    /// registration is refused -- the failure is logged and
    /// `SubscriberId::MAX`, never a live id, is returned.
    pub fn register(&self, sub: Box<dyn MemorySubscriber>) -> SubscriberId {
        match lock_or_err(&self.subs) {
            Ok(mut subs) => {
                subs.push(Arc::from(sub));
                subs.len() - 1
            }
            Err(e) => {
                tracing::error!(error = %e, "subscriber registry lock poisoned; registration refused");
                SubscriberId::MAX
            }
        }
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
            self.metrics
                .recursion_blocks
                .fetch_add(1, Ordering::Relaxed);
            let _ = self.bp_tx.send(BackpressureEvent::RecursionRefused {
                token_allows_emit: false,
            });
            tracing::debug!("pulse refused: caller holds a recursion token at depth >= 1");
            return;
        }
        // Task 12: a durability-qualified pulse is appended to the outbox
        // synchronously, before queue dispatch. This is the Class A path
        // (audit publishers): the guarded operation has already executed,
        // so a failed append cannot fail it retroactively — the failure is
        // counted and logged inside `append_qualified`, and the pulse
        // still dispatches. The error is deliberately discarded here;
        // publishers that must fail on a failed append use `emit_checked`.
        // The successful classes travel with the queued pulse: if a class's
        // append failed, its subscriber is not skipped by the dispatcher
        // and this queue delivery is the fallback that keeps the event.
        let (appended_classes, _) = self.append_qualified(&event);
        let evicted = self.queue.push(event, appended_classes, self.capacity);
        self.metrics.events_emitted.fetch_add(1, Ordering::Relaxed);
        if evicted {
            self.note_drop();
        }
    }

    /// Publish one pulse whose durability the caller must know about —
    /// the Class M path (mesh-state publishers on memory-write paths).
    /// These pulses *are* the sync record, so a failed durable append
    /// fails the operation: the error returns before the pulse enters the
    /// queue, and a write that cannot be durably recorded does not report
    /// success. On success this is `emit` with a receipt. A recursion
    /// refusal is an error here too (counted and published exactly as in
    /// `emit`), rather than a silent drop the caller cannot see.
    pub fn emit_checked(
        &self,
        event: ThalamicPulse,
        token: &RecursionToken,
    ) -> Result<(), OutboxError> {
        if !token.allows_emit() {
            self.metrics
                .recursion_blocks
                .fetch_add(1, Ordering::Relaxed);
            let _ = self.bp_tx.send(BackpressureEvent::RecursionRefused {
                token_allows_emit: false,
            });
            tracing::debug!("pulse refused: caller holds a recursion token at depth >= 1");
            return Err(OutboxError::RecursionRefused);
        }
        let (appended_classes, append_error) = self.append_qualified(&event);
        if let Some(err) = append_error {
            return Err(err);
        }
        let evicted = self.queue.push(event, appended_classes, self.capacity);
        self.metrics.events_emitted.fetch_add(1, Ordering::Relaxed);
        if evicted {
            self.note_drop();
        }
        Ok(())
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
            // Poison disposition: the snapshot reports 0 subscribers,
            // with the failure logged.
            subscribers: match lock_or_err(&self.subs) {
                Ok(subs) => subs.len(),
                Err(e) => {
                    tracing::error!(error = %e, "subscriber registry lock poisoned; snapshot reports 0 subscribers");
                    0
                }
            },
            queue_depth: self.queue.len(),
            drops_oldest: self.metrics.drops_oldest.load(Ordering::Relaxed),
            subscriber_panics: self.metrics.subscriber_panics.load(Ordering::Relaxed),
            subscriber_timeouts: self.metrics.subscriber_timeouts.load(Ordering::Relaxed),
            recursion_blocks: self.metrics.recursion_blocks.load(Ordering::Relaxed),
            outbox_append_failures: self
                .metrics
                .outbox_append_failures
                .load(Ordering::Relaxed),
            outbox_attached: self.metrics.outbox_attached.load(Ordering::Relaxed),
            outbox_attach_failed: self
                .metrics
                .outbox_attach_failed
                .load(Ordering::Relaxed),
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
    outbox: Arc<Mutex<Option<Arc<EventOutbox>>>>,
) {
    loop {
        let queued = match queue.pop() {
            Some(queued) => queued,
            None => {
                // Park until the next push. `notify_one`'s stored permit means
                // a push between the failed pop and this await is not missed.
                queue.notify.notified().await;
                continue;
            }
        };
        let QueuedPulse {
            event,
            appended_classes,
        } = queued;

        let namespace = namespace_of(&event);
        let ts = chrono::Utc::now().timestamp();
        let store: Arc<dyn VectorStore> = store
            .get()
            .cloned()
            .unwrap_or_else(|| Arc::new(StoreNotAttached));
        let ctx = SubscriberContext {
            store: store.as_ref(),
            namespace,
            ts,
        };

        // Poison disposition: this pulse is delivered to no one (the
        // dispatcher outlives the registry; killing it would silence every
        // later pulse too), with the failure logged.
        let registered: Vec<Arc<dyn MemorySubscriber>> = match lock_or_err(&subs) {
            Ok(subs) => subs.clone(),
            Err(e) => {
                tracing::error!(error = %e, "subscriber registry lock poisoned; pulse delivered to no one");
                Vec::new()
            }
        };
        // Task 12: with an outbox attached, a durability class's own
        // subscriber is *not* invoked here for a pulse its class is
        // interested in **when that pulse's append for the class
        // succeeded** — its delivery worker delivers it from the outbox,
        // in sequence order, without this dispatcher's 250 ms budget. A
        // failed append leaves no outbox entry, so the subscriber keeps
        // this ordinary queue delivery as the event's fallback. Every
        // other subscriber still sees the pulse here, exactly as before.
        let durable = outbox
            .lock()
            .expect("outbox slot lock poisoned")
            .is_some();
        for sub in registered.iter() {
            if durable {
                if let Some(class) = EventClass::class_of_subscriber(sub.name()) {
                    if class.interested_in(&event)
                        && appended_classes.contains(&class)
                    {
                        continue;
                    }
                }
            }
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

/// One class's delivery worker (Task 12): the outbox's reader. Two of
/// these run — audit and mesh — so audit delivery never queues behind
/// mesh deliveries performing git operations.
///
/// The worker delivers its class's unacknowledged entries in sequence
/// order and advances the class offset only after the class's subscriber
/// returns. The contract is **at-least-once**: a subscriber failure, a
/// panic, or a failed offset write leaves the offset where it was, and
/// the entry is delivered again — after a short backoff in-process, or
/// on the next start after a crash. There is deliberately **no**
/// per-delivery timeout here: the 250 ms dispatcher budget is the loss
/// this design exists to remove, and a slow mesh delivery (git work is
/// budgeted at 30 s by DR-4) must be allowed to finish. A hung
/// subscriber stalls only its own class.
///
/// Startup replay is not a separate mechanism: it is this loop's first
/// pass over the entries a previous run left unacknowledged.
async fn delivery_worker(
    class: EventClass,
    outbox: Arc<EventOutbox>,
    subs: Arc<Mutex<Vec<Arc<dyn MemorySubscriber>>>>,
    store: Arc<OnceLock<Arc<dyn VectorStore>>>,
    metrics: Arc<AtomicMetrics>,
    bp_tx: watch::Sender<BackpressureEvent>,
    notify: Arc<Notify>,
) {
    loop {
        let head = outbox.pending(class).into_iter().next();
        let Some(entry) = head else {
            // Nothing pending: park until an append wakes us. The bounded
            // poll is a backstop — a missed wakeup delays a delivery by
            // at most the poll interval, and can never lose one, because
            // the entry is durable and the loop re-reads `pending`.
            let _ =
                tokio::time::timeout(Duration::from_millis(250), notify.notified()).await;
            continue;
        };
        let targets: Vec<Arc<dyn MemorySubscriber>> = subs
            .lock()
            .expect("subscriber registry lock poisoned")
            .iter()
            .filter(|s| EventClass::class_of_subscriber(s.name()) == Some(class))
            .cloned()
            .collect();
        if targets.is_empty() {
            // Nobody is registered for this class in this process (the
            // mesh is not wired, say, though a previous run appended
            // entries for it). The entries stay unacknowledged: wedging
            // is honest and visible, dropping them would be loss.
            let _ =
                tokio::time::timeout(Duration::from_millis(250), notify.notified()).await;
            continue;
        }
        if deliver_entry(&entry, &targets, &store, &metrics, &bp_tx).await {
            match outbox.advance_offset(class, entry.seq) {
                Ok(()) => {}
                Err(err) => {
                    // The side effect landed but the acknowledgment did
                    // not: the entry *will* be redelivered, and the
                    // subscriber's idempotency basis absorbs the dup.
                    tracing::error!(
                        class = class.as_str(),
                        seq = entry.seq,
                        "outbox offset advance failed: {err}; the entry will be redelivered"
                    );
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        } else {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

/// Deliver one outbox entry to every subscriber of its class. Returns
/// whether *all* of them returned `Ok` — the offset may advance only
/// then. A panic is caught and counted exactly as the dispatcher counts
/// it, and counts as a failure: the entry stays unacknowledged.
async fn deliver_entry(
    entry: &super::outbox::OutboxEntry,
    targets: &[Arc<dyn MemorySubscriber>],
    store: &Arc<OnceLock<Arc<dyn VectorStore>>>,
    metrics: &Arc<AtomicMetrics>,
    bp_tx: &watch::Sender<BackpressureEvent>,
) -> bool {
    let namespace = namespace_of(&entry.pulse);
    let ts = chrono::Utc::now().timestamp();
    let store: Arc<dyn VectorStore> = store
        .get()
        .cloned()
        .unwrap_or_else(|| Arc::new(StoreNotAttached));
    let ctx = SubscriberContext {
        store: store.as_ref(),
        namespace,
        ts,
    };
    let mut all_ok = true;
    for sub in targets {
        // A fresh depth-1 token per subscriber, as in the dispatcher: a
        // subscriber may not emit from inside its own invocation.
        let token = RecursionToken::new();
        let outcome = AssertUnwindSafe(sub.handle(&entry.pulse, &token, &ctx))
            .catch_unwind()
            .await;
        match outcome {
            Ok(Ok(())) => {}
            Ok(Err(err)) => {
                tracing::warn!(
                    subscriber = sub.name(),
                    "outbox delivery failed: {err:#}"
                );
                all_ok = false;
            }
            Err(payload) => {
                metrics.subscriber_panics.fetch_add(1, Ordering::Relaxed);
                let text = panic_text(payload.as_ref());
                let _ = bp_tx.send(BackpressureEvent::SubscriberPanic {
                    name: sub.name(),
                    payload: redact_panic(&text),
                });
                tracing::warn!(subscriber = sub.name(), "outbox delivery panicked: {text}");
                all_ok = false;
            }
        }
    }
    all_ok
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
        bus.register(Box::new(SlowSubscriber {
            log: log2,
            delay: Duration::from_millis(200),
        }));
        for i in 0..5 {
            bus.emit(
                ThalamicPulse::Archived {
                    id: format!("{i}"),
                    namespace: "n".into(),
                },
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
            text: format!(
                "boom\t\u{7}{}\nSECOND LINE of a stack trace",
                "y".repeat(1000)
            ),
        }));
        bus.emit(
            ThalamicPulse::Created {
                id: "x".into(),
                namespace: "n".into(),
                kind: "task".into(),
            },
            &RecursionToken::root(),
        );
        tokio::time::sleep(Duration::from_millis(50)).await;

        let received = bp.borrow().clone();
        let BackpressureEvent::SubscriberPanic { payload, .. } = &received else {
            panic!("expected SubscriberPanic on the watch channel, got {received:?}");
        };
        assert!(
            payload.starts_with("boom"),
            "the first line survives: {payload}"
        );
        assert!(
            !payload.contains('\t') && !payload.contains('\u{7}'),
            "control chars stripped"
        );
        assert!(!payload.contains('\n'), "only the first line may travel");
        assert!(
            !payload.contains("SECOND LINE"),
            "stack-trace context must not leak"
        );
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
            ThalamicPulse::Created {
                id: "x".into(),
                namespace: "n".into(),
                kind: "task".into(),
            },
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
            ThalamicPulse::Created {
                id: "x".into(),
                namespace: "n".into(),
                kind: "task".into(),
            },
            &RecursionToken::new(),
        );
        tokio::time::sleep(Duration::from_millis(50)).await;

        let m = bus.metrics();
        assert_eq!(
            m.recursion_blocks, 1,
            "blocked emit must count a recursion block: {:?}",
            m
        );
        assert_eq!(
            m.events_emitted, 0,
            "blocked emit must not enter the queue: {:?}",
            m
        );
        assert_eq!(
            ran.load(Ordering::Acquire),
            0,
            "no subscriber may see a blocked pulse"
        );
        assert_eq!(
            *bp.borrow(),
            BackpressureEvent::RecursionRefused {
                token_allows_emit: false
            },
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
        bus.register(Box::new(Sleeper {
            delay: Duration::from_millis(500),
        }));
        bus.register(Box::new(CountingSubscriber { counter: ran2 }));
        let bp = bus.backpressure();

        bus.emit(
            ThalamicPulse::Created {
                id: "x".into(),
                namespace: "n".into(),
                kind: "task".into(),
            },
            &RecursionToken::root(),
        );
        // Past the 250ms budget, before the sleeper's own 500ms would end.
        tokio::time::sleep(Duration::from_millis(400)).await;

        let m = bus.metrics();
        assert_eq!(
            m.subscriber_timeouts, 1,
            "slow subscriber must be abandoned once: {:?}",
            m
        );
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
            ThalamicPulse::Created {
                id: "x".into(),
                namespace: "n".into(),
                kind: "task".into(),
            },
            &RecursionToken::root(),
        );
        // give dispatcher time
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            rec.load(Ordering::Acquire),
            1,
            "subscriber ran exactly once"
        );
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
        ) -> anyhow::Result<crate::traits::UpsertReport> {
            Err(anyhow!("recording store: unused by this test"))
        }
        async fn search(
            &self,
            _namespace: &str,
            _vector: Vec<f32>,
            _limit: usize,
            _include_tasks: bool,
            _include_archived: bool,
        ) -> anyhow::Result<Vec<SearchResult>> {
            Err(anyhow!("recording store: unused by this test"))
        }
        async fn delete(&self, _namespace: &str, _id: &str) -> anyhow::Result<()> {
            Err(anyhow!("recording store: unused by this test"))
        }
        async fn clear_ingested(&self, _namespace: &str) -> anyhow::Result<()> {
            Err(anyhow!("recording store: unused by this test"))
        }
        async fn relink_edges(&self, _namespace: &str, _source_key: &str) -> anyhow::Result<usize> {
            Err(anyhow!("recording store: unused by this test"))
        }
        async fn list(
            &self,
            _namespace: &str,
            _user_id: Option<&str>,
            _memory_type: Option<&str>,
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
        async fn export_graph(
            &self,
            _include_retired: bool,
            _include_archived: bool,
            _project_contexts: &std::collections::HashMap<String, std::path::PathBuf>,
            _anchor_override: Option<&crate::source_context::ScopedAnchorOverride>,
        ) -> anyhow::Result<serde_json::Value> {
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

        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

impl crate::traits::AsAny for RecordingStore {
    fn as_any(&self) -> &dyn std::any::Any {
        self
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
        bus.register(Box::new(StoreProbingSubscriber {
            oks: oks.clone(),
            errs: errs.clone(),
        }));
        bus.emit(
            ThalamicPulse::Created {
                id: "x".into(),
                namespace: "n".into(),
                kind: "task".into(),
            },
            &RecursionToken::root(),
        );
        tokio::time::sleep(Duration::from_millis(50)).await;

        assert_eq!(
            oks.load(Ordering::Acquire),
            1,
            "the attached store must answer the probe"
        );
        assert_eq!(
            errs.load(Ordering::Acquire),
            0,
            "the stand-in must not be in the context"
        );
        assert_eq!(
            gets.load(Ordering::Acquire),
            1,
            "the call reached the attached instance"
        );
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
        bus.attach_store(Arc::new(RecordingStore {
            gets: first.clone(),
        }));
        bus.attach_store(Arc::new(RecordingStore {
            gets: second.clone(),
        }));
        let oks = Arc::new(AtomicUsize::new(0));
        let errs = Arc::new(AtomicUsize::new(0));
        bus.register(Box::new(StoreProbingSubscriber {
            oks: oks.clone(),
            errs: errs.clone(),
        }));
        bus.emit(
            ThalamicPulse::Created {
                id: "x".into(),
                namespace: "n".into(),
                kind: "task".into(),
            },
            &RecursionToken::root(),
        );
        tokio::time::sleep(Duration::from_millis(50)).await;

        assert_eq!(
            first.load(Ordering::Acquire),
            1,
            "the first attach is the store in ctx.store"
        );
        assert_eq!(
            second.load(Ordering::Acquire),
            0,
            "the second attach must be a no-op"
        );
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
        bus.register(Box::new(StoreProbingSubscriber {
            oks: oks.clone(),
            errs: errs.clone(),
        }));
        bus.emit(
            ThalamicPulse::Created {
                id: "x".into(),
                namespace: "n".into(),
                kind: "task".into(),
            },
            &RecursionToken::root(),
        );
        tokio::time::sleep(Duration::from_millis(50)).await;

        assert_eq!(
            errs.load(Ordering::Acquire),
            1,
            "the stand-in must answer explicitly"
        );
        assert_eq!(
            oks.load(Ordering::Acquire),
            0,
            "nothing may read through an unattached store"
        );
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
            ThalamicPulse::Created {
                id: "x".into(),
                namespace: "n".into(),
                kind: "task".into(),
            },
            &RecursionToken::root(),
        );
        let m = bus.metrics();
        assert_eq!(m.events_emitted, 1);
        assert_eq!(m.drops_oldest, 1);
        assert_eq!(m.queue_depth, 0);
        assert_eq!(*bp.borrow(), BackpressureEvent::Dropped { count: 1 });
    }

    // ---- Task 12: durable outbox for audit & mesh events ----

    use crate::events::{EventClass, EventOutbox, GuardEventLog};
    use crate::store::LadybugStore;
    use std::collections::HashSet;
    use std::path::PathBuf;

    /// A fresh outbox directory per test, named after the test, so a leftover
    /// from an earlier run can never be read back as this run's output.
    fn outbox_scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "neurostrata-bus-outbox-test-{}-{}-{}",
            std::process::id(),
            tag,
            uuid::Uuid::new_v4()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn pulse_label(event: &ThalamicPulse) -> String {
        match event {
            ThalamicPulse::Created { id, .. } => format!("created:{id}"),
            ThalamicPulse::Superseded { old_id, new_id, .. } => {
                format!("superseded:{old_id}->{new_id}")
            }
            ThalamicPulse::Archived { id, .. } => format!("archived:{id}"),
            ThalamicPulse::GuardedActionFired { trace_id, .. } => format!("guard:{trace_id}"),
        }
    }

    /// The record identity a delivery is absorbed by: the memory id for mesh
    /// pulses (one mesh record per id — the mesh worker's dedup basis), the
    /// trace id for a guard call.
    fn pulse_identity(event: &ThalamicPulse) -> String {
        match event {
            ThalamicPulse::Created { id, .. } => id.clone(),
            ThalamicPulse::Superseded { old_id, .. } => old_id.clone(),
            ThalamicPulse::Archived { id, .. } => id.clone(),
            ThalamicPulse::GuardedActionFired { trace_id, .. } => trace_id.clone(),
        }
    }

    /// A durability-class subscriber double. It carries one of the two real
    /// subscriber names so the bus classes it exactly as production does, and
    /// — like the real subscribers — it records only the pulses its class is
    /// interested in (the audit log ignores storage mutations; the mesh
    /// outbox ignores guard verdicts). Every delivery is recorded in order,
    /// and the identity set mirrors the idempotency basis a real subscriber
    /// absorbs duplicates with.
    struct ClassRecorder {
        sub_name: &'static str,
        delivered: Arc<Mutex<Vec<String>>>,
        identities: Arc<Mutex<HashSet<String>>>,
        delay: Duration,
    }

    impl ClassRecorder {
        fn new(
            sub_name: &'static str,
            delivered: Arc<Mutex<Vec<String>>>,
            identities: Arc<Mutex<HashSet<String>>>,
        ) -> Self {
            Self {
                sub_name,
                delivered,
                identities,
                delay: Duration::ZERO,
            }
        }
    }

    #[async_trait]
    impl MemorySubscriber for ClassRecorder {
        fn name(&self) -> &'static str {
            self.sub_name
        }

        async fn handle(
            &self,
            event: &ThalamicPulse,
            _token: &RecursionToken,
            _ctx: &SubscriberContext<'_>,
        ) -> Result<(), SubscriberError> {
            let interested = match self.sub_name {
                "guard_event_log" => {
                    matches!(event, ThalamicPulse::GuardedActionFired { .. })
                }
                "mesh_outbox" => {
                    !matches!(event, ThalamicPulse::GuardedActionFired { .. })
                }
                _ => true,
            };
            if !interested {
                return Ok(());
            }
            if !self.delay.is_zero() {
                tokio::time::sleep(self.delay).await;
            }
            self.delivered
                .lock()
                .expect("delivered log poisoned")
                .push(pulse_label(event));
            self.identities
                .lock()
                .expect("identity set poisoned")
                .insert(pulse_identity(event));
            Ok(())
        }
    }

    /// A class subscriber that never returns: the worker delivering to it is
    /// stuck mid-entry, so offsets can never advance — the shape of a daemon
    /// that took a pulse into the outbox and died before delivering it.
    struct BlockingClassSub {
        sub_name: &'static str,
    }

    #[async_trait]
    impl MemorySubscriber for BlockingClassSub {
        fn name(&self) -> &'static str {
            self.sub_name
        }

        async fn handle(
            &self,
            _event: &ThalamicPulse,
            _token: &RecursionToken,
            _ctx: &SubscriberContext<'_>,
        ) -> Result<(), SubscriberError> {
            std::future::pending::<()>().await;
            Ok(())
        }
    }

    /// Poll `cond` every 10ms until it holds or the deadline passes: the
    /// workers deliver on their own tasks, so a guessed sleep could not tell
    /// "hasn't happened yet" from "will never happen".
    async fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !cond() {
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for {what}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    fn outbox_lines(dir: &std::path::Path) -> usize {
        match std::fs::read_to_string(dir.join("event-outbox.jsonl")) {
            Ok(text) => text.lines().filter(|l| !l.trim().is_empty()).count(),
            Err(_) => 0,
        }
    }

    /// (audit, mesh) offsets as the offsets file currently records them; a
    /// missing or unreadable file reads as (0, 0) — nothing acknowledged.
    fn offsets_at(dir: &std::path::Path) -> (u64, u64) {
        let Ok(text) = std::fs::read_to_string(dir.join("event-outbox.offsets.json")) else {
            return (0, 0);
        };
        let value: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
        (
            value["audit"].as_u64().unwrap_or(0),
            value["mesh"].as_u64().unwrap_or(0),
        )
    }

    fn created(id: &str, namespace: &str) -> ThalamicPulse {
        ThalamicPulse::Created {
            id: id.to_string(),
            namespace: namespace.to_string(),
            kind: "fact".to_string(),
        }
    }

    fn created_pulse() -> ThalamicPulse {
        ThalamicPulse::Created {
            id: "x".to_string(),
            namespace: "ns".to_string(),
            kind: "fact".to_string(),
        }
    }

    fn guarded(trace_id: &str, namespace: &str) -> ThalamicPulse {
        ThalamicPulse::GuardedActionFired {
            trace_id: trace_id.to_string(),
            action_type: "shell".to_string(),
            payload_hash: 0xDEAD_BEEF,
            verdict: "deny".to_string(),
            rule_ids_triggered: vec!["rule.no-rm-rf".to_string()],
            namespace: namespace.to_string(),
        }
    }

    /// A current-thread runtime for the restart tests: each phase gets its
    /// own, and dropping it kills that phase's bus tasks — the crash.
    fn test_runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a test runtime builds")
    }

    #[tokio::test]
    async fn audit_event_survives_bus_overflow() {
        // The real audit subscriber against a real store — the wiring the
        // daemon builds, so the survival claim is about production parts.
        let dir = outbox_scratch("overflow");
        let store_dir = outbox_scratch("overflow-store");
        let store = Arc::new(
            LadybugStore::for_testing(
                store_dir.join("audit.lbug").to_string_lossy().to_string(),
                4,
            )
            .expect("a throwaway store opens"),
        );
        store.init("audit-ns").await.expect("the namespace table exists");

        let bus = ThalamicBus::new(2);
        bus.attach_store(store.clone());
        bus.attach_event_outbox(dir);
        // A slow class-O subscriber stalls the dispatcher 200ms per pulse,
        // so the capacity-2 queue genuinely overflows behind it.
        bus.register(Box::new(SlowSubscriber {
            log: Arc::new(Mutex::new(Vec::new())),
            delay: Duration::from_millis(200),
        }));
        bus.register(Box::new(GuardEventLog::new(4)));

        bus.emit(created("c0", "audit-ns"), &RecursionToken::root());
        for i in 1..=5 {
            bus.emit(guarded(&format!("t{i}"), "audit-ns"), &RecursionToken::root());
        }

        // The queue could only hold two of the five audit pulses and
        // drop-oldest evicted the rest — yet every one must land as an audit
        // row, delivered in order by the outbox worker.
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let rows = loop {
            let rows: Vec<crate::traits::MemoryPayload> = store
                .list("audit-ns", None, None)
                .await
                .expect("the namespace lists")
                .into_iter()
                .map(|r| r.payload)
                .filter(|p| p.memory_type == "guard_event")
                .collect();
            if rows.len() == 5 {
                break rows;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "expected 5 audit rows after overflow, saw {}",
                rows.len()
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        let traces: HashSet<String> = rows
            .iter()
            .map(|p| {
                p.metadata["trace_id"]
                    .as_str()
                    .expect("an audit row names its trace")
                    .to_string()
            })
            .collect();
        assert_eq!(
            traces,
            HashSet::from([
                "t1".to_string(),
                "t2".to_string(),
                "t3".to_string(),
                "t4".to_string(),
                "t5".to_string()
            ]),
            "every flooded-out audit pulse still left its row"
        );
        assert!(
            bus.metrics().drops_oldest >= 3,
            "the queue really did overflow and drop: {:?}",
            bus.metrics()
        );
    }

    #[tokio::test]
    async fn slow_subscriber_does_not_lose_mesh_event() {
        let dir = outbox_scratch("slow-mesh");
        let bus = ThalamicBus::new(16);
        bus.attach_event_outbox(dir);
        let delivered = Arc::new(Mutex::new(Vec::new()));
        let identities = Arc::new(Mutex::new(HashSet::new()));
        bus.register(Box::new(ClassRecorder {
            sub_name: "mesh_outbox",
            delivered: delivered.clone(),
            identities: identities.clone(),
            // Past the dispatcher's 250ms per-subscriber budget: on the old
            // path this delivery was abandoned and the event was gone.
            delay: Duration::from_millis(400),
        }));
        bus.emit(created("m1", "mesh-ns"), &RecursionToken::root());

        let d = delivered.clone();
        wait_until("the slow mesh delivery", move || {
            d.lock().unwrap().len() == 1
        })
        .await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            *delivered.lock().unwrap(),
            vec!["created:m1".to_string()],
            "exactly one delivery — not zero (lost to the timeout) and not two"
        );
        let m = bus.metrics();
        assert_eq!(
            m.subscriber_timeouts, 0,
            "the dispatcher's 250ms budget never abandoned it: {m:?}"
        );
        assert_eq!(m.drops_oldest, 0);
    }

    #[test]
    fn unreplayed_outbox_entries_replay_on_startup() {
        let dir = outbox_scratch("replay");

        // Phase 1: a daemon that appends three qualified pulses and dies
        // before delivering any of them (both class subscribers hang, so no
        // offset can advance).
        test_runtime().block_on(async {
            let bus = ThalamicBus::new(16);
            bus.attach_event_outbox(dir.clone());
            bus.register(Box::new(BlockingClassSub {
                sub_name: "mesh_outbox",
            }));
            bus.register(Box::new(BlockingClassSub {
                sub_name: "guard_event_log",
            }));
            bus.emit(created("m1", "mesh-ns"), &RecursionToken::root());
            bus.emit(created("m2", "mesh-ns"), &RecursionToken::root());
            bus.emit(guarded("g1", "audit-ns"), &RecursionToken::root());
            let d = dir.clone();
            wait_until("three appended outbox entries", move || {
                outbox_lines(&d) == 3
            })
            .await;
            assert_eq!(
                offsets_at(&dir),
                (0, 0),
                "nothing was delivered, so no offset may advance"
            );
        });

        // Phase 2: the restart replays what phase 1 left behind, per class
        // and in sequence order, and the offsets advance past it.
        let mesh_delivered = Arc::new(Mutex::new(Vec::new()));
        let audit_delivered = Arc::new(Mutex::new(Vec::new()));
        test_runtime().block_on(async {
            let bus = ThalamicBus::new(16);
            bus.attach_event_outbox(dir.clone());
            bus.register(Box::new(ClassRecorder::new(
                "mesh_outbox",
                mesh_delivered.clone(),
                Arc::new(Mutex::new(HashSet::new())),
            )));
            bus.register(Box::new(ClassRecorder::new(
                "guard_event_log",
                audit_delivered.clone(),
                Arc::new(Mutex::new(HashSet::new())),
            )));
            let (md, ad) = (mesh_delivered.clone(), audit_delivered.clone());
            wait_until("replay of both classes", move || {
                md.lock().unwrap().len() == 2 && ad.lock().unwrap().len() == 1
            })
            .await;
            let d = dir.clone();
            wait_until("offsets advanced past the replay", move || {
                offsets_at(&d) == (3, 2)
            })
            .await;
        });
        assert_eq!(
            *mesh_delivered.lock().unwrap(),
            vec!["created:m1".to_string(), "created:m2".to_string()],
            "mesh entries replay in sequence order"
        );
        assert_eq!(
            *audit_delivered.lock().unwrap(),
            vec!["guard:g1".to_string()]
        );

        // Phase 3: a second restart has nothing left to replay — the offsets
        // held, so nothing is delivered twice.
        let mesh2 = Arc::new(Mutex::new(Vec::new()));
        let audit2 = Arc::new(Mutex::new(Vec::new()));
        test_runtime().block_on(async {
            let bus = ThalamicBus::new(16);
            bus.attach_event_outbox(dir.clone());
            bus.register(Box::new(ClassRecorder::new(
                "mesh_outbox",
                mesh2.clone(),
                Arc::new(Mutex::new(HashSet::new())),
            )));
            bus.register(Box::new(ClassRecorder::new(
                "guard_event_log",
                audit2.clone(),
                Arc::new(Mutex::new(HashSet::new())),
            )));
            tokio::time::sleep(Duration::from_millis(400)).await;
        });
        assert!(
            mesh2.lock().unwrap().is_empty(),
            "no duplicate mesh delivery on the second restart"
        );
        assert!(
            audit2.lock().unwrap().is_empty(),
            "no duplicate audit delivery on the second restart"
        );
    }

    #[test]
    fn crash_between_effect_and_offset_redelivers() {
        let dir = outbox_scratch("crash");
        let delivered = Arc::new(Mutex::new(Vec::new()));
        let identities = Arc::new(Mutex::new(HashSet::new()));

        // Sabotage the offsets file — a directory where it belongs — so
        // every offset advance fails *after* the subscriber's side effect
        // has landed: the crash point, held open.
        std::fs::create_dir_all(dir.join("event-outbox.offsets.json")).unwrap();

        test_runtime().block_on(async {
            let bus = ThalamicBus::new(16);
            bus.attach_event_outbox(dir.clone());
            bus.register(Box::new(ClassRecorder::new(
                "mesh_outbox",
                delivered.clone(),
                identities.clone(),
            )));
            bus.emit(created("m1", "mesh-ns"), &RecursionToken::root());
            let d = delivered.clone();
            wait_until("the side effect lands", move || {
                !d.lock().unwrap().is_empty()
            })
            .await;
            assert_eq!(
                offsets_at(&dir),
                (0, 0),
                "the offset advance is fault-blocked, so the entry stays unacknowledged"
            );
        });

        // The crash: the runtime and its workers die with the entry
        // delivered-but-unacknowledged. The operator clears the fault.
        std::fs::remove_dir_all(dir.join("event-outbox.offsets.json")).unwrap();
        let before = delivered.lock().unwrap().len();
        assert!(before >= 1, "the first delivery happened");

        test_runtime().block_on(async {
            let bus = ThalamicBus::new(16);
            bus.attach_event_outbox(dir.clone());
            bus.register(Box::new(ClassRecorder::new(
                "mesh_outbox",
                delivered.clone(),
                identities.clone(),
            )));
            let d = delivered.clone();
            wait_until("the entry redelivers after restart", move || {
                d.lock().unwrap().len() > before
            })
            .await;
            let dd = dir.clone();
            wait_until("the offset advances once the fault clears", move || {
                offsets_at(&dd).1 >= 1
            })
            .await;
        });

        // At-least-once, as assertions: the pulse was delivered again after
        // the restart, and the duplicate was absorbed by the subscriber's
        // idempotency basis — one record identity, however many deliveries.
        assert!(
            delivered.lock().unwrap().len() > before,
            "redelivery happened after the restart"
        );
        assert_eq!(
            *identities.lock().unwrap(),
            HashSet::from(["m1".to_string()]),
            "the duplicate is absorbed by record identity"
        );
    }

    #[tokio::test]
    async fn mesh_publish_append_failure_fails_the_operation() {
        let dir = outbox_scratch("mesh-append-fail");
        let bus = ThalamicBus::new(16);
        bus.attach_event_outbox(dir.clone());
        let delivered = Arc::new(Mutex::new(Vec::new()));
        bus.register(Box::new(ClassRecorder::new(
            "mesh_outbox",
            delivered.clone(),
            Arc::new(Mutex::new(HashSet::new())),
        )));

        // Sabotage the append itself: a directory where the outbox file
        // belongs, so the durable write fails with a real io error.
        let _ = std::fs::remove_file(dir.join("event-outbox.jsonl"));
        std::fs::create_dir_all(dir.join("event-outbox.jsonl")).unwrap();

        let result = bus.emit_checked(created("m1", "mesh-ns"), &RecursionToken::root());
        assert!(
            result.is_err(),
            "a mesh publish whose durable append failed must fail the operation"
        );
        assert_eq!(
            bus.metrics().events_emitted,
            0,
            "the failed publish never entered the queue"
        );
        assert_eq!(
            bus.metrics().outbox_append_failures,
            1,
            "the failure is counted, not silent"
        );
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(
            delivered.lock().unwrap().is_empty(),
            "nothing was delivered for the failed publish"
        );
    }

    #[tokio::test]
    async fn audit_append_failure_alerts_and_continues() {
        let dir = outbox_scratch("audit-append-fail");
        let bus = ThalamicBus::new(16);
        bus.attach_event_outbox(dir.clone());
        bus.register(Box::new(ClassRecorder::new(
            "guard_event_log",
            Arc::new(Mutex::new(Vec::new())),
            Arc::new(Mutex::new(HashSet::new())),
        )));
        let _ = std::fs::remove_file(dir.join("event-outbox.jsonl"));
        std::fs::create_dir_all(dir.join("event-outbox.jsonl")).unwrap();

        // Class A disposition: the guarded operation has already executed,
        // so `emit` returns and the pulse is still accepted by the bus —
        // the failure's record is the counter on the metrics surface.
        bus.emit(guarded("g1", "audit-ns"), &RecursionToken::root());
        assert_eq!(
            bus.metrics().outbox_append_failures,
            1,
            "the failure counter is the alert surface"
        );
        assert_eq!(
            bus.metrics().events_emitted,
            1,
            "the operation continued: the pulse was accepted"
        );
    }

    #[tokio::test]
    async fn audit_append_failure_falls_back_to_queue_delivery() {
        // The real audit subscriber against a real store: when the durable
        // append fails, the queue — not an outbox worker — must deliver
        // this pulse to GuardEventLog, restoring the pre-Task-12
        // best-effort path for exactly the pulse that has no outbox entry.
        let dir = outbox_scratch("audit-append-fallback");
        let store_dir = outbox_scratch("audit-append-fallback-store");
        let store = Arc::new(
            LadybugStore::for_testing(
                store_dir.join("audit.lbug").to_string_lossy().to_string(),
                4,
            )
            .expect("a throwaway store opens"),
        );
        store
            .init("audit-ns")
            .await
            .expect("the namespace table exists");
        let bus = ThalamicBus::new(16);
        bus.attach_store(store.clone());
        bus.attach_event_outbox(dir.clone());
        bus.register(Box::new(GuardEventLog::new(4)));

        let _ = std::fs::remove_file(dir.join("event-outbox.jsonl"));
        std::fs::create_dir_all(dir.join("event-outbox.jsonl")).unwrap();
        bus.emit(guarded("fallback-trace", "audit-ns"), &RecursionToken::root());
        assert_eq!(
            bus.metrics().outbox_append_failures,
            1,
            "the failed append is counted before the fallback runs"
        );

        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let rows: Vec<crate::traits::MemoryPayload> = store
                .list("audit-ns", None, None)
                .await
                .expect("the namespace lists")
                .into_iter()
                .map(|r| r.payload)
                .filter(|p| {
                    p.memory_type == "guard_event"
                        && p.metadata["trace_id"].as_str() == Some("fallback-trace")
                })
                .collect();
            if rows.len() == 1 {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "expected the queue fallback to write one audit row, saw {}",
                rows.len()
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[test]
    fn offset_advance_failure_redelivers_in_process_after_fault_clears() {
        let dir = outbox_scratch("offset-retry-in-process");
        // Hold the crash point open inside one process: the side effect
        // lands, but every offsets write fails because a directory sits
        // where the offsets file belongs.
        std::fs::create_dir_all(dir.join("event-outbox.offsets.json")).unwrap();
        let delivered = Arc::new(Mutex::new(Vec::new()));
        let identities = Arc::new(Mutex::new(HashSet::new()));

        test_runtime().block_on(async {
            let bus = ThalamicBus::new(16);
            bus.attach_event_outbox(dir.clone());
            bus.register(Box::new(ClassRecorder::new(
                "mesh_outbox",
                delivered.clone(),
                identities.clone(),
            )));
            bus.emit(created("m1", "mesh-ns"), &RecursionToken::root());
            let d = delivered.clone();
            wait_until("the first delivery despite the blocked offset", move || {
                !d.lock().unwrap().is_empty()
            })
            .await;
            assert_eq!(
                offsets_at(&dir),
                (0, 0),
                "the offset is still blocked by the filesystem fault"
            );

            // The operator clears the fault; the same bus, with no
            // restart, must retry the still-pending entry and advance.
            std::fs::remove_dir_all(dir.join("event-outbox.offsets.json")).unwrap();
            let dd = dir.clone();
            wait_until("the offset advances without a restart", move || {
                offsets_at(&dd).1 >= 1
            })
            .await;
        });

        assert!(
            delivered.lock().unwrap().len() >= 2,
            "the entry redelivered in-process once the fault cleared: {:?}",
            delivered.lock().unwrap()
        );
        assert_eq!(
            *identities.lock().unwrap(),
            HashSet::from(["m1".to_string()]),
            "redelivery is absorbed by the same record identity"
        );
    }

    #[test]
    fn attach_failure_is_visible_in_metrics() {
        let dir = outbox_scratch("attach-failure-state");
        let blocked = dir.join("not-a-directory");
        std::fs::File::create(&blocked).unwrap();

        let bus = ThalamicBus::new(16);
        let before = bus.metrics();
        assert!(!before.outbox_attached);
        assert!(
            !before.outbox_attach_failed,
            "an unattached bus is not a failed attach"
        );

        bus.attach_event_outbox(blocked);
        let after = bus.metrics();
        assert!(!after.outbox_attached);
        assert!(
            after.outbox_attach_failed,
            "metrics must distinguish a failed attach from no attach"
        );

        let healthy_dir = outbox_scratch("attach-success-state");
        let healthy = ThalamicBus::new(16);
        healthy.attach_event_outbox(healthy_dir);
        let healthy_metrics = healthy.metrics();
        assert!(healthy_metrics.outbox_attached);
        assert!(!healthy_metrics.outbox_attach_failed);
    }

    #[test]
    fn outbox_compaction_bounds_the_file() {
        let dir = outbox_scratch("compaction");
        let ob = EventOutbox::open(&dir).expect("the outbox opens");
        for i in 0..10 {
            ob.append(EventClass::Mesh, &created(&format!("m{i}"), "mesh-ns"))
                .unwrap();
        }
        for i in 0..2 {
            ob.append(EventClass::Audit, &guarded(&format!("g{i}"), "audit-ns"))
                .unwrap();
        }
        ob.advance_offset(EventClass::Mesh, 10).unwrap();
        ob.advance_offset(EventClass::Audit, 12).unwrap();
        // 10 of 12 entries are past *both* offsets — over the dead-ratio
        // line, so compaction keeps only the unacknowledged.
        ob.compact().unwrap();
        assert!(ob.pending(EventClass::Mesh).is_empty());
        assert!(ob.pending(EventClass::Audit).is_empty());
        // The sequence base survives the rewrite: the next append does not
        // restart at 1 and collide with the recorded offsets.
        let seq = ob
            .append(EventClass::Mesh, &created("m10", "mesh-ns"))
            .unwrap();
        assert_eq!(seq, 13, "compaction preserves the sequence base");
        drop(ob);

        // The size trigger, on the startup path: a file past 8 MiB whose
        // entries are all acknowledged is compacted away when the outbox is
        // opened — no append is needed to bound it.
        let dir2 = outbox_scratch("compaction-size");
        let big = "x".repeat(3_000_000);
        {
            let ob2 = EventOutbox::open(&dir2).expect("the outbox opens");
            for _ in 0..3 {
                let mut p = created("big", "mesh-ns");
                if let ThalamicPulse::Created { kind, .. } = &mut p {
                    *kind = big.clone();
                }
                ob2.append(EventClass::Mesh, &p).unwrap();
            }
            ob2.advance_offset(EventClass::Mesh, 3).unwrap();
            ob2.advance_offset(EventClass::Audit, 3).unwrap();
            let size = std::fs::metadata(dir2.join("event-outbox.jsonl"))
                .unwrap()
                .len();
            assert!(
                size > 8 * 1024 * 1024,
                "the fixture really is past the bound: {size}"
            );
        }
        let ob2 = EventOutbox::open(&dir2).expect("the outbox reopens");
        assert!(
            ob2.pending(EventClass::Mesh).is_empty(),
            "startup compaction kept no acknowledged entry"
        );
        let size = std::fs::metadata(dir2.join("event-outbox.jsonl"))
            .unwrap()
            .len();
        assert!(
            size < 1024 * 1024,
            "startup compaction shrank the file: {size}"
        );
    }

    #[tokio::test]
    async fn other_event_classes_still_drop_oldest() {
        // No durability-interested subscriber is registered, so every pulse
        // here is Class O: even with an outbox attached, the bounded bus
        // behaves exactly as it did before Task 12 (pin).
        let dir = outbox_scratch("class-o");
        let bus = ThalamicBus::new(2);
        bus.attach_event_outbox(dir.clone());
        let log = Arc::new(Mutex::new(Vec::new()));
        bus.register(Box::new(SlowSubscriber {
            log: log.clone(),
            delay: Duration::from_millis(200),
        }));
        for i in 0..5 {
            bus.emit(
                ThalamicPulse::Archived {
                    id: format!("{i}"),
                    namespace: "n".into(),
                },
                &RecursionToken::root(),
            );
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(
            *log.lock().unwrap(),
            vec!["3".to_string(), "4".to_string()],
            "drop-oldest is unchanged for non-qualified traffic"
        );
        assert_eq!(bus.metrics().drops_oldest, 3);
        assert_eq!(
            outbox_lines(&dir),
            0,
            "nothing was durability-qualified, so the outbox stays empty"
        );
    }

    /// Panic in a scoped thread while it holds `mutex`: the mutex is
    /// poisoned when the thread unwinds out of the guard. The join swallows
    /// the deliberate panic; the poison is the point.
    fn poison<T: Send + 'static>(mutex: &Mutex<T>) {
        std::thread::scope(|scope| {
            let _ = scope
                .spawn(|| {
                    let _guard = mutex.lock().unwrap();
                    panic!("deliberate poison for the lock-policy test");
                })
                .join();
        });
    }

    #[test]
    fn poisoned_queue_degrades_not_panics() {
        let queue = Queue {
            items: Mutex::new(VecDeque::new()),
            notify: Notify::new(),
        };
        poison(&queue.items);
        assert!(
            queue.push(created_pulse(), Vec::new(), 4),
            "a poisoned queue counts the unqueueable pulse as dropped"
        );
        assert!(queue.pop().is_none(), "a poisoned queue pops as empty");
        assert_eq!(queue.len(), 0, "a poisoned queue reports depth 0");
    }

    #[test]
    fn poisoned_registry_degrades_not_panics() {
        // Outside a runtime the bus is headless (no dispatcher); register
        // and metrics do not need one.
        let bus = ThalamicBus::new(4);
        poison(&bus.subs);
        let id = bus.register(Box::new(SlowSubscriber {
            log: Arc::new(Mutex::new(Vec::new())),
            delay: Duration::from_millis(0),
        }));
        assert_eq!(
            id,
            SubscriberId::MAX,
            "a poisoned registry returns the invalid id, logged, not a panic"
        );
        assert_eq!(
            bus.metrics().subscribers,
            0,
            "a poisoned registry snapshots as zero subscribers"
        );
    }

    #[tokio::test]
    async fn poisoned_registry_snapshot_skips_one_pulse_not_the_dispatcher() {
        let queue = Arc::new(Queue {
            items: Mutex::new(VecDeque::new()),
            notify: Notify::new(),
        });
        let subs: Arc<Mutex<Vec<Arc<dyn MemorySubscriber>>>> =
            Arc::new(Mutex::new(Vec::new()));
        poison(&subs);
        let metrics = Arc::new(AtomicMetrics::default());
        let (bp_tx, _) = watch::channel(BackpressureEvent::Idle);
        let store = Arc::new(OnceLock::new());
        let handle = tokio::spawn(dispatch(
            queue.clone(),
            subs,
            metrics.clone(),
            bp_tx,
            store,
            Arc::new(Mutex::new(None)),
        ));
        queue.push(created_pulse(), Vec::new(), 4);
        for _ in 0..100 {
            if metrics.dispatcher_handled.load(Ordering::Relaxed) == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(
            metrics.dispatcher_handled.load(Ordering::Relaxed),
            1,
            "the pulse is accounted for, its delivery skipped"
        );
        assert!(
            !handle.is_finished(),
            "the dispatcher survives a poisoned registry"
        );
        handle.abort();
    }
}
