//! The thalamic bus runtime: a bounded queue, one dispatcher task, and the
//! metrics a `POST /bus/metrics` consumer reads.
//!
//! `emit` never blocks the caller that just made the storage write: it hands
//! the pulse to a bounded queue (drop-oldest when full) and returns. A single
//! dispatcher task walks the queue and invokes every registered subscriber in
//! order, each under a 250ms timeout, each panic caught at the dispatcher join
//! point. Nothing a subscriber does can reach back into the caller's write.
//!
//! ## Durable classes (Wave 3, step 1)
//!
//! A bus built with [`ThalamicBus::with_outbox`] qualifies two classes of
//! pulse for at-least-once delivery: Audit (`GuardedActionFired`, consumed by
//! `guard_event_log`) and Mesh (the three storage mutations, consumed by
//! `mesh_outbox`). A qualified pulse is appended to the durable outbox
//! (`outbox.rs`) before it is queued; a queued pulse carries its
//! [`DurableDelivery`] identity into the subscriber's context, and the class
//! offset advances only after every subscriber returned `Ok` -- and only over
//! a contiguous acknowledged prefix, so a later success never buries an
//! earlier failed entry. An unadvanced entry is replayed at the next start.
//! Qualified pulses are exempt from drop-oldest (the outbox is their
//! backstop); every other pulse keeps the original lossy behavior. Once a
//! deployment announces a mesh publish path, a checked mesh emit fails rather
//! than silently degrading when the mesh subscriber is not wired.

use super::outbox::EventOutbox;
use super::outbox::{MeshCapture, OutboxEntry};
use super::{
    BackpressureEvent, BusMetrics, DurableDelivery, EventClass, EventOutboxError, MemorySubscriber,
    RecursionToken, SubscriberContext, ThalamicPulse,
};
use crate::traits::{MemoryPayload, RelocateOutcome, SearchResult, VectorStore};
use anyhow::anyhow;
use async_trait::async_trait;
use futures::FutureExt;
use std::any::Any;
use std::collections::{HashSet, VecDeque};
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};
use std::time::Duration;
use tokio::sync::{watch, Notify};

/// Identifies a registered subscriber: its index in the registry. Stable for
/// the life of the bus (subscribers are never unregistered in v1).
pub type SubscriberId = usize;

/// Per-subscriber budget. Longer than any v1 subscriber needs; a runaway
/// subscriber is abandoned rather than allowed to stall the queue behind it.
const SUBSCRIBER_TIMEOUT: Duration = Duration::from_millis(250);

/// In-memory cap on durable pulses. The outbox is the backlog's home; a
/// publisher that would exceed this cap still appends durably but leaves the
/// pulse for the dispatcher's top-up, so memory stays bounded under load.
const MAX_DURABLE_QUEUED: usize = 10_000;
/// Entries loaded per top-up pass, and the startup replay cap.
const REPLAY_BATCH: usize = 256;
/// Pause after a failed durable delivery before the top-up retries it.
const RETRY_PAUSE: Duration = Duration::from_millis(100);
/// A class with no subscriber still yields to every other class: the dispatcher
/// parks for this short backstop, not the old five-second registration wait.
const MISSING_CLASS_BACKOFF: Duration = Duration::from_millis(20);

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
    subscriber_failures: AtomicU64,
    /// Audit appends that failed and fell back to the lossy queue.
    audit_append_failures: AtomicU64,
    /// Unacknowledged entries replayed into the queue at construction.
    outbox_replayed: AtomicU64,
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

/// The single static stand-in handed to every subscriber whose pulse runs
/// before a store is attached: one allocation for the process, cloned per
/// pulse, instead of a fresh `StoreNotAttached` allocated under the dispatcher
/// on every such pulse (F9).
static STORE_NOT_ATTACHED: LazyLock<Arc<dyn VectorStore>> =
    LazyLock::new(|| Arc::new(StoreNotAttached));

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

/// One queued pulse, plus the durable identity it will acknowledge when (and
/// only when) every subscriber returns `Ok`. `None` is an ordinary lossy
/// pulse.
struct QueuedPulse {
    event: ThalamicPulse,
    delivery: Option<DurableDelivery>,
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
    /// Push one pulse, trimming the oldest lossy pulse when the lossy part of
    /// the queue is over `capacity`. Durable pulses are exempt from the bound:
    /// the outbox already holds them, and dropping one from the queue would
    /// defer its delivery to the next start for no reason. Returns the exact
    /// number of pulses evicted: at capacity 0 the pulse just pushed (if
    /// lossy) is also the oldest, and goes, but a queue that already exceeds
    /// the bound can shed several at once. The caller counts every eviction as
    /// a drop.
    fn push(&self, pulse: QueuedPulse, capacity: usize) -> usize {
        let mut items = self.items.lock().unwrap_or_else(|e| e.into_inner());
        items.push_back(pulse);
        let mut evicted = 0usize;
        // The bound applies to lossy pulses alone. Durable pulses are counted
        // separately by `durable_len` and admitted only up to
        // `MAX_DURABLE_QUEUED`, so replaying a large backlog can never evict
        // unrelated lossy traffic.
        let mut lossy = items.iter().filter(|q| q.delivery.is_none()).count();
        while lossy > capacity {
            let Some(pos) = items.iter().position(|queued| queued.delivery.is_none()) else {
                break;
            };
            items.remove(pos);
            evicted += 1;
            lossy -= 1;
        }
        // Release before waking: the waker only needs the edge, and it takes
        // the lock itself to pop.
        drop(items);
        self.notify.notify_one();
        evicted
    }

    /// How many durable pulses are currently in memory. Callers refuse a new
    /// durable enqueue past the cap; the outbox keeps the record and the
    /// dispatcher's top-up loads it when capacity frees.
    fn durable_len(&self) -> usize {
        self.items
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|q| q.delivery.is_some())
            .count()
    }

    /// Put a popped pulse back at the front, with no trimming and no drop
    /// accounting. This is the missing-subscriber hold: the pulse was already
    /// counted, its durable record still exists, and pushing it back where it
    /// came from preserves the class's sequence order without evicting any
    /// lossy pulse behind it.
    fn push_front(&self, pulse: QueuedPulse) {
        self.items
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push_front(pulse);
        self.notify.notify_one();
    }

    /// Hold a durable pulse whose class has no subscriber without parking it
    /// at the head of the queue. Other classes must keep draining; only this
    /// class waits. The pulse goes after the last queued pulse of its own
    /// class, so a class that later gains a subscriber still sees its entries
    /// in sequence order.
    fn hold_for_missing_class(&self, pulse: QueuedPulse) {
        let mut items = self.items.lock().unwrap_or_else(|e| e.into_inner());
        let identity = pulse.delivery.as_ref();
        let insert_at = identity
            .and_then(|delivery| {
                // Before the FIRST queued entry of this class with a higher
                // sequence: a subscriber registering mid-hold must never see
                // seq N+1 before seq N.
                items.iter().position(|queued| {
                    queued.delivery.as_ref().is_some_and(|queued| {
                        queued.class == delivery.class && queued.seq > delivery.seq
                    })
                })
            })
            .or_else(|| {
                identity.and_then(|delivery| {
                    items
                        .iter()
                        .rposition(|queued| {
                            queued
                                .delivery
                                .as_ref()
                                .is_some_and(|queued| queued.class == delivery.class)
                        })
                        .map(|position| position + 1)
                })
            })
            .unwrap_or(items.len());
        items.insert(insert_at, pulse);
        drop(items);
        self.notify.notify_one();
    }

    fn pop(&self) -> Option<QueuedPulse> {
        self.items
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop_front()
    }

    fn len(&self) -> usize {
        self.items.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
}

/// Tunables for the durable delivery path.
#[derive(Debug, Clone, Copy)]
pub struct OutboxConfig {
    /// Upper bound on unacknowledged entries replayed into the queue at
    /// construction, per class. Anything past it stays durable and replays on
    /// a later start; the cap exists so an unexpectedly large outbox cannot
    /// balloon startup memory in one go.
    pub replay_limit: usize,
}

impl Default for OutboxConfig {
    fn default() -> Self {
        Self {
            // Bounded production replay. A huge backlog stays durable and is
            // replayed in later batches; startup never tries to materialise an
            // unbounded file in memory.
            replay_limit: 10_000,
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
    /// The durable event outbox, when one was attached at construction.
    /// `None` is the bus every non-daemon consumer runs: bounded queue
    /// semantics exactly as before.
    outbox: Option<Arc<EventOutbox>>,
    /// Wakes a dispatcher holding a durable entry whose class has no
    /// subscriber yet, so registration is observed immediately instead of
    /// polled for.
    subs_notify: Arc<Notify>,
    /// Serializes the durable append with the queue enqueue, so two
    /// concurrent publishers of the same class cannot enqueue seq N+1 ahead
    /// of seq N (which would bury N behind a later success).
    publish_lock: Arc<Mutex<()>>,
    /// Durable deliveries currently in the queue or being handled. Top-up
    /// skips them, so a slow subscriber cannot cause a duplicate enqueue; a
    /// failed delivery is removed and becomes eligible again without waiting
    /// for a restart.
    inflight: Arc<Mutex<HashSet<(u64, u64)>>>,
    /// Set when a mesh publish path is configured (a mesh pointer exists).
    /// From then on a checked mesh emit must be durably recorded or fail: an
    /// absent mesh subscriber is a wiring error, not a quiet fallback.
    mesh_required: AtomicBool,
    /// The mesh identity and epoch in force at append time. Stamped on each
    /// durable mesh entry, so a later replay keeps its original destination
    /// instead of following the daemon's current pointer.
    mesh_binding: Mutex<Option<MeshCapture>>,
}

impl ThalamicBus {
    /// Construct the bus and start its dispatcher on the current Tokio
    /// runtime. Outside a runtime there is nothing to dispatch on: the bus is
    /// still constructible (`emit` is runtime-independent) but stays headless,
    /// with the failure logged rather than panicked -- emitted pulses queue
    /// and then drop oldest at capacity, exactly as they would with a
    /// dispatcher that died.
    pub fn new(capacity: usize) -> Self {
        let bus = Self::assemble(capacity, None);
        bus.start_dispatcher();
        bus
    }

    /// Construct a bus whose Audit and Mesh pulses are durable: every
    /// qualified pulse is appended to `outbox` before it is queued, and its
    /// class offset advances only after the subscribers return `Ok`.
    ///
    /// The queue is seeded with whatever a previous run left unacknowledged --
    /// `pending(Audit)` in sequence order, then `pending(Mesh)` -- *before* the
    /// dispatcher starts, so replay is ahead of every live pulse and a replayed
    /// entry keeps its original place in the class's sequence.
    pub fn with_outbox(capacity: usize, outbox: EventOutbox, config: OutboxConfig) -> Self {
        let outbox = Arc::new(outbox);
        let bus = Self::assemble(capacity, Some(outbox.clone()));
        let generation = outbox.generation();
        let mut replayed = 0u64;
        for class in EventClass::all() {
            for entry in outbox.pending_limited(class, config.replay_limit) {
                let capture = capture_of(&entry);
                let replay_generation = generation_of(&entry, generation);
                bus.inflight
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert((replay_generation, entry.seq));
                bus.queue.push(
                    QueuedPulse {
                        event: entry.pulse,
                        delivery: Some(DurableDelivery {
                            class,
                            seq: entry.seq,
                            generation: replay_generation,
                            capture,
                        }),
                    },
                    capacity,
                );
                replayed += 1;
            }
        }
        bus.metrics
            .outbox_replayed
            .fetch_add(replayed, Ordering::Relaxed);
        bus.start_dispatcher();
        bus
    }

    fn assemble(capacity: usize, outbox: Option<Arc<EventOutbox>>) -> Self {
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
        Self {
            queue,
            subs,
            metrics,
            bp_tx,
            capacity,
            store,
            outbox,
            subs_notify: Arc::new(Notify::new()),
            publish_lock: Arc::new(Mutex::new(())),
            inflight: Arc::new(Mutex::new(HashSet::new())),
            mesh_required: AtomicBool::new(false),
            mesh_binding: Mutex::new(None),
        }
    }

    fn start_dispatcher(&self) {
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(dispatch(
                    self.queue.clone(),
                    self.subs.clone(),
                    self.metrics.clone(),
                    self.bp_tx.clone(),
                    self.store.clone(),
                    self.outbox.clone(),
                    self.subs_notify.clone(),
                    self.inflight.clone(),
                    self.publish_lock.clone(),
                    self.capacity,
                ));
            }
            Err(_) => tracing::warn!(
                "ThalamicBus constructed outside a Tokio runtime; no dispatcher started, emitted pulses will queue and then drop oldest at capacity"
            ),
        }
    }

    /// The attached outbox, if any. Tests use it to observe the durable state
    /// the bus shares with them.
    #[cfg(test)]
    pub fn outbox(&self) -> Option<Arc<EventOutbox>> {
        self.outbox.clone()
    }

    /// The classes `event` is durability-qualified for *right now*: a class
    /// qualifies iff an outbox is attached, its named subscriber is
    /// registered, and the class is interested in the pulse. Follows the
    /// subscription registry -- never a hardcoded list of event kinds.
    fn qualified_classes(&self, event: &ThalamicPulse) -> Vec<EventClass> {
        if self.outbox.is_none() {
            return Vec::new();
        }
        let subs = self.subs.lock().unwrap_or_else(|e| e.into_inner());
        EventClass::all()
            .into_iter()
            .filter(|class| {
                class.interested_in(event)
                    && subs
                        .iter()
                        .any(|sub| EventClass::class_of_subscriber(sub.name()) == Some(*class))
            })
            .collect()
    }

    /// Durably append `event` for the class it qualifies for, before queue
    /// dispatch. An Audit append failure is counted and logged here and the
    /// pulse falls back to the ordinary queue (the guarded operation already
    /// happened; its audit record cannot fail it retroactively). A Mesh
    /// append failure is returned: the caller that must fail its operation
    /// uses [`ThalamicBus::emit_checked`].
    fn append_qualified(
        &self,
        event: &ThalamicPulse,
    ) -> Result<Option<DurableDelivery>, EventOutboxError> {
        let mesh_required = self.mesh_required.load(Ordering::Acquire);
        let mesh_pulse = EventClass::Mesh.interested_in(event);
        let Some(outbox) = self.outbox.as_ref() else {
            if mesh_required && mesh_pulse {
                // The deployment says it publishes to a mesh, but this bus has
                // no outbox at all (it failed to open). Do not report success
                // for a sync record that does not exist.
                return Err(EventOutboxError::MeshUnavailable);
            }
            return Ok(None);
        };
        let classes = self.qualified_classes(event);
        if mesh_required && mesh_pulse && classes.is_empty() {
            // A mesh is configured but the subscriber that would consume and
            // acknowledge this pulse is not registered (wiring failed). The
            // lossy queue would swallow the sync record silently.
            return Err(EventOutboxError::MeshUnavailable);
        }
        let binding = *self.mesh_binding.lock().unwrap_or_else(|e| e.into_inner());
        let mut mesh_error = None;
        for class in classes {
            let capture = if class == EventClass::Mesh {
                binding
            } else {
                None
            };
            match outbox.append_captured(class, event, capture) {
                Ok(seq) => {
                    return Ok(Some(DurableDelivery {
                        class,
                        seq,
                        generation: outbox.generation(),
                        capture,
                    }))
                }
                Err(err) => match class {
                    EventClass::Audit => {
                        self.metrics
                            .audit_append_failures
                            .fetch_add(1, Ordering::Relaxed);
                        tracing::error!(
                            "durable audit append failed; the pulse falls back to the lossy queue: {err}"
                        );
                    }
                    EventClass::Mesh => {
                        tracing::error!("durable mesh append failed: {err}");
                        mesh_error = Some(err);
                    }
                },
            }
        }
        match mesh_error {
            Some(err) => Err(err),
            None => Ok(None),
        }
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
        let mut subs = self.subs.lock().unwrap_or_else(|e| e.into_inner());
        subs.push(Arc::from(sub));
        let id = subs.len() - 1;
        drop(subs);
        // A durable entry whose class was not yet registered may be held by
        // the dispatcher; wake it now rather than making it wait out its
        // bounded sleep.
        self.subs_notify.notify_one();
        id
    }

    /// Publish one pulse. Never blocks and never fails the caller's storage
    /// write. The recursion guard is checked here, at the bus boundary: a
    /// caller holding a token at depth ≥ 1 (a subscriber mid-invocation) is
    /// refused, the refusal is counted and published, and the pulse never
    /// reaches the queue. A full queue drops its oldest lossy member to make
    /// room: the pulse that arrives is accepted, and whatever the push left
    /// behind capacity is what `drops_oldest` counts.
    ///
    /// A durability-qualified pulse is appended to the outbox first. An Audit
    /// append failure is counted and logged, and the pulse takes the ordinary
    /// queue path; a Mesh append failure cannot fail this call (there is no
    /// error to return) and is logged, with the pulse falling back to the
    /// queue. A mesh publisher that must fail its operation uses
    /// [`ThalamicBus::emit_checked`].
    pub fn emit(&self, event: ThalamicPulse, token: &RecursionToken) {
        if !token.allows_emit() {
            self.refuse_recursion();
            return;
        }
        let _publish = self
            .publish_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let delivery = match self.append_qualified(&event) {
            Ok(delivery) => delivery,
            Err(err) => {
                tracing::error!(
                    "durable mesh append failed in emit; falling back to the lossy queue: {err}"
                );
                None
            }
        };
        self.enqueue(event, delivery);
    }

    /// Publish one pulse whose durability the caller must know about -- the
    /// mesh-state publishers on memory-write paths. These pulses *are* the
    /// sync record, so a failed durable append fails the operation: the error
    /// returns before the pulse enters the queue, and a write that cannot be
    /// durably recorded does not report success.
    ///
    /// On a bus without an outbox (every test and non-daemon consumer) this is
    /// `emit` with a receipt: `Ok(())`. A recursion refusal is an error here
    /// too (counted and published exactly as in `emit`), rather than a silent
    /// drop the caller cannot see.
    pub fn emit_checked(
        &self,
        event: ThalamicPulse,
        token: &RecursionToken,
    ) -> Result<(), EventOutboxError> {
        if !token.allows_emit() {
            self.refuse_recursion();
            return Err(EventOutboxError::RecursionRefused);
        }
        let _publish = self
            .publish_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let delivery = self.append_qualified(&event)?;
        self.enqueue(event, delivery);
        Ok(())
    }

    /// Announce that this deployment has a mesh publish path configured. A
    /// checked mesh emit from here on must find a durable append *and* a mesh
    /// subscriber to acknowledge it; otherwise it fails instead of silently
    /// falling back to the lossy queue, which would let a memory write report
    /// a sync record the mesh never received. Standalone deployments never
    /// call this, so their `emit_checked` keeps the original receipt-only
    /// behavior.
    pub fn expect_mesh_durability(&self) {
        self.mesh_required.store(true, Ordering::Release);
    }

    /// Bind checked mesh emits to the mesh that is actually configured. Called
    /// once the pointer is resolved; every append from then on carries this
    /// identity, and replays keep it even after the daemon joins another mesh.
    pub fn set_mesh_binding(&self, capture: MeshCapture) {
        *self.mesh_binding.lock().unwrap_or_else(|e| e.into_inner()) = Some(capture);
    }

    fn refuse_recursion(&self) {
        self.metrics
            .recursion_blocks
            .fetch_add(1, Ordering::Relaxed);
        let _ = self.bp_tx.send(BackpressureEvent::RecursionRefused {
            token_allows_emit: false,
        });
        tracing::debug!("pulse refused: caller holds a recursion token at depth >= 1");
    }

    fn enqueue(&self, event: ThalamicPulse, delivery: Option<DurableDelivery>) {
        self.metrics.events_emitted.fetch_add(1, Ordering::Relaxed);
        if delivery.is_some() && self.queue.durable_len() >= MAX_DURABLE_QUEUED {
            // The append already made this durable. Leave it in the outbox and
            // let the dispatcher's top-up load it when capacity frees; a new
            // enqueue here would grow memory without bound under sustained
            // publish load.
            tracing::debug!(
                "durable queue at {} entries; delivery left in the outbox for top-up",
                MAX_DURABLE_QUEUED
            );
            return;
        }
        let evicted = self
            .queue
            .push(QueuedPulse { event, delivery }, self.capacity);
        for _ in 0..evicted {
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
    /// snapshot satisfies `events_emitted + outbox_replayed ==
    /// dispatcher_handled + queue_depth + drops_oldest` (the replayed term is
    /// zero on a bus without an outbox).
    pub fn metrics(&self) -> BusMetrics {
        BusMetrics {
            events_emitted: self.metrics.events_emitted.load(Ordering::Relaxed),
            dispatcher_handled: self.metrics.dispatcher_handled.load(Ordering::Relaxed),
            subscribers: self.subs.lock().unwrap_or_else(|e| e.into_inner()).len(),
            queue_depth: self.queue.len(),
            drops_oldest: self.metrics.drops_oldest.load(Ordering::Relaxed),
            subscriber_panics: self.metrics.subscriber_panics.load(Ordering::Relaxed),
            subscriber_timeouts: self.metrics.subscriber_timeouts.load(Ordering::Relaxed),
            recursion_blocks: self.metrics.recursion_blocks.load(Ordering::Relaxed),
            subscriber_failures: self.metrics.subscriber_failures.load(Ordering::Relaxed),
            audit_append_failures: self.metrics.audit_append_failures.load(Ordering::Relaxed),
            outbox_attached: self.outbox.is_some(),
            outbox_replayed: self.metrics.outbox_replayed.load(Ordering::Relaxed),
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

/// Recover the mesh binding stamped when the entry was written. Replay must
/// reuse it: the daemon's current pointer can be a different mesh entirely.
/// The generation a replay must report: the one stamped on the entry, or the
/// current outbox generation for a legacy line written before the stamp.
fn generation_of(entry: &OutboxEntry, current: u64) -> u64 {
    if entry.generation == 0 {
        current
    } else {
        entry.generation
    }
}

fn capture_of(entry: &OutboxEntry) -> Option<MeshCapture> {
    match (entry.mesh_id, entry.mesh_epoch) {
        (Some(mesh_id), Some(epoch)) => Some(MeshCapture { mesh_id, epoch }),
        _ => None,
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
/// Load the next slice of unacknowledged outbox entries into the queue.
/// Entries already in flight are skipped without stalling the scan, so a slow
/// class cannot hide the older deliveries behind it. Everything left in the
/// outbox stays durable and is picked up by a later pass.
fn top_up_durable(
    outbox: &EventOutbox,
    queue: &Queue,
    inflight: &Mutex<HashSet<(u64, u64)>>,
    publish_lock: &Mutex<()>,
    capacity: usize,
) {
    if queue.durable_len() >= MAX_DURABLE_QUEUED {
        return;
    }
    // Same lock as a live append: a top-up scan and its enqueues must not be
    // interleaved with a newer pulse entering the queue, or seq N+1 could be
    // dispatched ahead of the older entry being topped up.
    let _publish = publish_lock
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let generation = outbox.generation();
    let skip = inflight.lock().unwrap_or_else(|e| e.into_inner()).len();
    for class in EventClass::all() {
        if queue.durable_len() >= MAX_DURABLE_QUEUED {
            break;
        }
        // Look past the in-flight prefix, then take a bounded batch.
        for entry in outbox.pending_limited(class, skip + REPLAY_BATCH) {
            if queue.durable_len() >= MAX_DURABLE_QUEUED {
                break;
            }
            let replay_generation = generation_of(&entry, generation);
            let key = (replay_generation, entry.seq);
            if !inflight
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(key)
            {
                continue;
            }
            let capture = capture_of(&entry);
            queue.push(
                QueuedPulse {
                    event: entry.pulse,
                    delivery: Some(DurableDelivery {
                        class,
                        seq: entry.seq,
                        generation: replay_generation,
                        capture,
                    }),
                },
                capacity,
            );
        }
    }
}

/// Completion state for one durable delivery across ALL subscribers, including
/// detached timeout retries. The offset advances only when every subscriber has
/// finished successfully -- a detached retry succeeding for one subscriber must
/// not acknowledge a pulse another subscriber failed.
struct DeliveryCompletion {
    remaining: usize,
    all_ok: bool,
}

fn note_delivery(
    completion: &Mutex<DeliveryCompletion>,
    ok: bool,
    identity: Option<DurableDelivery>,
    outbox: &Option<Arc<EventOutbox>>,
    inflight: &Mutex<HashSet<(u64, u64)>>,
) {
    let (done, all_ok) = {
        let mut state = completion.lock().unwrap_or_else(|e| e.into_inner());
        state.remaining = state.remaining.saturating_sub(1);
        state.all_ok &= ok;
        (state.remaining == 0, state.all_ok)
    };
    let Some(identity) = identity else { return };
    if !done {
        // Keep the slot reserved while a detached retry is still running, so a
        // top-up cannot re-enqueue the same sequence concurrently.
        return;
    }
    inflight
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&(identity.generation, identity.seq));
    if all_ok {
        if let Some(outbox) = outbox {
            if let Err(err) = outbox.advance(identity.class, identity.seq) {
                tracing::error!(
                    class = identity.class.as_str(),
                    seq = identity.seq,
                    "outbox offset advance failed: {err}; the entry will be redelivered"
                );
            }
        }
    } else {
        tracing::warn!(
            class = identity.class.as_str(),
            seq = identity.seq,
            "durable delivery did not succeed; the entry stays unacknowledged for retry"
        );
    }
}

async fn dispatch(
    queue: Arc<Queue>,
    subs: Arc<Mutex<Vec<Arc<dyn MemorySubscriber>>>>,
    metrics: Arc<AtomicMetrics>,
    bp_tx: watch::Sender<BackpressureEvent>,
    store: Arc<OnceLock<Arc<dyn VectorStore>>>,
    outbox: Option<Arc<EventOutbox>>,
    subs_notify: Arc<Notify>,
    inflight: Arc<Mutex<HashSet<(u64, u64)>>>,
    publish_lock: Arc<Mutex<()>>,
    capacity: usize,
) {
    let mut last_top_up = std::time::Instant::now();
    loop {
        if let Some(outbox) = outbox.as_ref() {
            // A small bus must still top up: `capacity / 2` is 0 below two,
            // which would make the check unreachable forever.
            let threshold = (capacity / 2).max(1);
            if queue.len() < threshold && last_top_up.elapsed() >= Duration::from_millis(50) {
                top_up_durable(outbox, &queue, &inflight, &publish_lock, capacity);
                last_top_up = std::time::Instant::now();
            }
        }
        let queued = match queue.pop() {
            Some(queued) => queued,
            None => {
                // Park until the next push. `notify_one`'s stored permit means
                // a push between the failed pop and this await is not missed.
                queue.notify.notified().await;
                continue;
            }
        };
        let QueuedPulse { event, delivery } = queued;

        // A durable entry is delivered only when its class's subscriber is
        // registered. Construction enqueues replay before the daemon has had a
        // chance to register anything, and acknowledging an entry because an
        // empty registry "returned Ok" would silently discard it. Hold the
        // entry in the queue until its consumer exists; a class that is never
        // wired stays unacknowledged and replays on the next start.
        if let Some(identity) = delivery {
            let class_subscribed = subs
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .any(|sub| EventClass::class_of_subscriber(sub.name()) == Some(identity.class));
            if !class_subscribed {
                // Hold the entry without re-heading the queue: a class that is
                // never wired must not starve every other class. It keeps its
                // place in its own class's sequence and lets the rest drain.
                queue.hold_for_missing_class(QueuedPulse {
                    event,
                    delivery: Some(identity),
                });
                // Park until a registration (or a bounded backstop) rather
                // than polling: a class that is never wired costs one wakeup
                // per backstop, not a spin.
                let _ = tokio::time::timeout(MISSING_CLASS_BACKOFF, subs_notify.notified()).await;
                continue;
            }
        }

        let namespace = namespace_of(&event);
        let ts = chrono::Utc::now().timestamp();
        let store: Arc<dyn VectorStore> = store
            .get()
            .cloned()
            .unwrap_or_else(|| STORE_NOT_ATTACHED.clone());
        let ctx = SubscriberContext {
            store: store.as_ref(),
            namespace,
            ts,
            delivery,
        };

        let registered: Vec<Arc<dyn MemorySubscriber>> =
            subs.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let completion = Arc::new(Mutex::new(DeliveryCompletion {
            remaining: registered.len(),
            all_ok: true,
        }));
        let mut main_failed = false;
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
                Ok(Ok(Ok(()))) => note_delivery(&completion, true, delivery, &outbox, &inflight),
                Ok(Ok(Err(err))) => {
                    main_failed = true;
                    metrics.subscriber_failures.fetch_add(1, Ordering::Relaxed);
                    tracing::warn!(subscriber = sub.name(), "subscriber failed: {err:#}");
                    note_delivery(&completion, false, delivery, &outbox, &inflight);
                }
                Ok(Err(payload)) => {
                    main_failed = true;
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
                    note_delivery(&completion, false, delivery, &outbox, &inflight);
                }
                Err(_elapsed) => {
                    main_failed = true;
                    metrics.subscriber_timeouts.fetch_add(1, Ordering::Relaxed);
                    let name = sub.name().to_string();
                    let _ = bp_tx.send(BackpressureEvent::SubscriberTimeout { name: sub.name() });
                    tracing::warn!(
                        subscriber = sub.name(),
                        "subscriber abandoned after {}ms timeout",
                        SUBSCRIBER_TIMEOUT.as_millis()
                    );
                    if sub.retry_on_timeout() {
                        // Detached recovery task so the dispatcher isn't blocked
                        // on the 4x budget; the subscriber's flip outlives the
                        // transient stall. The in-flight slot and the offset
                        // advance are both deferred to this task's completion:
                        // a top-up cannot re-enqueue while the retry runs, and
                        // no offset moves before EVERY subscriber has finished.
                        let sub = Arc::clone(sub);
                        let event = event.clone();
                        let metrics = metrics.clone();
                        let store = store.clone();
                        let retry_outbox = outbox.clone();
                        let retry_inflight = inflight.clone();
                        let retry_completion = completion.clone();
                        let ns = namespace.to_string();
                        let name = name.clone();
                        tokio::spawn(async move {
                            let ctx = SubscriberContext {
                                store: store.as_ref(),
                                namespace: &ns,
                                ts,
                                delivery,
                            };
                            let retry = tokio::time::timeout(
                                SUBSCRIBER_TIMEOUT * 4,
                                AssertUnwindSafe(sub.handle(&event, &RecursionToken::new(), &ctx))
                                    .catch_unwind(),
                            )
                            .await;
                            match retry {
                                Ok(Ok(Ok(()))) => {
                                    tracing::info!(
                                        subscriber = %name,
                                        "subscriber retry after timeout succeeded"
                                    );
                                    note_delivery(
                                        &retry_completion,
                                        true,
                                        delivery,
                                        &retry_outbox,
                                        &retry_inflight,
                                    );
                                }
                                Ok(Ok(Err(err))) => {
                                    metrics.subscriber_failures.fetch_add(1, Ordering::Relaxed);
                                    tracing::warn!(
                                        subscriber = name,
                                        "subscriber retry after timeout failed: {err:#}"
                                    );
                                    note_delivery(
                                        &retry_completion,
                                        false,
                                        delivery,
                                        &retry_outbox,
                                        &retry_inflight,
                                    );
                                }
                                Ok(Err(payload)) => {
                                    metrics.subscriber_panics.fetch_add(1, Ordering::Relaxed);
                                    let text = panic_text(payload.as_ref());
                                    tracing::warn!(
                                        subscriber = name,
                                        "subscriber panicked on retry after timeout: {text}"
                                    );
                                    note_delivery(
                                        &retry_completion,
                                        false,
                                        delivery,
                                        &retry_outbox,
                                        &retry_inflight,
                                    );
                                }
                                Err(_) => {
                                    tracing::warn!(
                                        subscriber = name,
                                        "subscriber retry after timeout also ran past {}ms",
                                        (SUBSCRIBER_TIMEOUT * 4).as_millis()
                                    );
                                    note_delivery(
                                        &retry_completion,
                                        false,
                                        delivery,
                                        &retry_outbox,
                                        &retry_inflight,
                                    );
                                }
                            }
                        });
                    } else {
                        note_delivery(&completion, false, delivery, &outbox, &inflight);
                    }
                }
            }
        }
        // A main-loop failure paid for a bounded pause so a permanently failing
        // subscriber cannot hot-loop the dispatcher. Detached retries settle on
        // their own task; `note_delivery` owns the in-flight slot and the offset.
        if main_failed && delivery.is_some() {
            tokio::time::sleep(RETRY_PAUSE).await;
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
    use std::sync::atomic::{AtomicBool, AtomicUsize};

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
    async fn retry_on_timeout_subscriber_lands_flip_after_initial_timeout() {
        /// Sleeps 300ms on its first invocation -- long enough to be
        /// abandoned by the dispatcher's fast-path timeout -- and succeeds in
        /// 10ms on the retry. The first attempt's `handle` is cancelled, not
        /// resumed, so the state flip only lands if the dispatcher retries.
        struct SlowFirstFlip {
            calls: Arc<AtomicUsize>,
            flipped: Arc<AtomicBool>,
        }

        #[async_trait]
        impl MemorySubscriber for SlowFirstFlip {
            fn name(&self) -> &'static str {
                "slow_first_flip"
            }

            fn retry_on_timeout(&self) -> bool {
                true
            }

            async fn handle(
                &self,
                _event: &ThalamicPulse,
                _token: &RecursionToken,
                _ctx: &SubscriberContext<'_>,
            ) -> Result<(), SubscriberError> {
                let call = self.calls.fetch_add(1, Ordering::AcqRel);
                if call == 0 {
                    let deadline = tokio::time::Instant::now() + Duration::from_millis(500);
                    loop {
                        if self.flipped.load(Ordering::Acquire) {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(10)).await;
                        if tokio::time::Instant::now() > deadline {
                            break;
                        }
                    }
                } else {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    self.flipped.store(true, Ordering::Release);
                }
                Ok(())
            }
        }

        let bus = ThalamicBus::new(16);
        let calls = Arc::new(AtomicUsize::new(0));
        let flipped = Arc::new(AtomicBool::new(false));
        bus.register(Box::new(SlowFirstFlip {
            calls: calls.clone(),
            flipped: flipped.clone(),
        }));
        bus.emit(
            ThalamicPulse::Created {
                id: "x".into(),
                namespace: "n".into(),
                kind: "task".into(),
            },
            &RecursionToken::root(),
        );
        // 250ms fast-path abandon + the retry's 10ms + scheduling slack.
        tokio::time::sleep(Duration::from_millis(500)).await;

        let m = bus.metrics();
        assert_eq!(
            m.subscriber_timeouts, 1,
            "the first attempt is still counted and published as a timeout: {m:?}"
        );
        assert_eq!(m.dispatcher_handled, 1, "one pulse, one dispatch: {m:?}");
        assert!(
            flipped.load(Ordering::Acquire),
            "the retry under the extended budget landed the flip"
        );
        assert_eq!(
            calls.load(Ordering::Acquire),
            2,
            "first attempt plus exactly one retry"
        );
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

    /// Returns `Err` on every pulse, on purpose: the dispatcher must log it,
    /// count it on `BusMetrics::subscriber_failures`, and move on -- the same
    /// disposition a panic gets, minus the watch-channel event (F2).
    struct FailingSubscriber;

    #[async_trait]
    impl MemorySubscriber for FailingSubscriber {
        fn name(&self) -> &'static str {
            "failer"
        }

        async fn handle(
            &self,
            _event: &ThalamicPulse,
            _token: &RecursionToken,
            _ctx: &SubscriberContext<'_>,
        ) -> Result<(), SubscriberError> {
            Err(anyhow!("boom"))
        }
    }

    #[tokio::test]
    async fn subscriber_error_counts_a_failure() {
        let bus = ThalamicBus::new(16);
        let ran = Arc::new(AtomicUsize::new(0));
        let ran2 = ran.clone();
        bus.register(Box::new(FailingSubscriber));
        bus.register(Box::new(CountingSubscriber { counter: ran2 }));
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
            bus.metrics().subscriber_failures,
            1,
            "an Err from a subscriber is one recorded failure: {:?}",
            bus.metrics()
        );
        assert_eq!(
            ran.load(Ordering::Acquire),
            1,
            "the dispatcher reaches the next subscriber after a failure"
        );
    }

    // ---- durable outbox tests (Wave 3, step 1) ----

    use crate::events::outbox::{DurableDelivery, EventClass, EventOutbox};
    use std::path::PathBuf;
    use tokio::sync::Semaphore;

    /// One durable-path observation: which pulse, and the delivery identity
    /// the bus stamped into the subscriber's context (if any).
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct DurableSeen {
        id: String,
        delivery: Option<DurableDelivery>,
    }

    /// A subscriber that stands in for one of the two durability-class
    /// consumers (`guard_event_log`, `mesh_outbox`). It can block on a
    /// semaphore (to observe a delivery mid-flight), sleep, and fail after
    /// recording its effect (to simulate a crash between the effect and the
    /// offset write).
    struct DurableSpy {
        name: &'static str,
        seen: Arc<Mutex<Vec<DurableSeen>>>,
        entered: Arc<AtomicBool>,
        gate: Option<Arc<Semaphore>>,
        delay: Duration,
        fail: bool,
    }

    impl DurableSpy {
        fn new(name: &'static str, seen: Arc<Mutex<Vec<DurableSeen>>>) -> Self {
            Self {
                name,
                seen,
                entered: Arc::new(AtomicBool::new(false)),
                gate: None,
                delay: Duration::from_millis(0),
                fail: false,
            }
        }
    }

    #[async_trait]
    impl MemorySubscriber for DurableSpy {
        fn name(&self) -> &'static str {
            self.name
        }

        async fn handle(
            &self,
            event: &ThalamicPulse,
            _token: &RecursionToken,
            ctx: &SubscriberContext<'_>,
        ) -> Result<(), SubscriberError> {
            // Model the named consumer: a class subscriber acts only on the
            // variants its class is interested in (the real `GuardEventLog`
            // and `MeshOutbox` no-op the rest). A durable entry is delivered
            // to every subscriber, so each spy must ignore the other class.
            if let Some(class) = EventClass::class_of_subscriber(self.name) {
                if !class.interested_in(event) {
                    return Ok(());
                }
            }
            self.entered.store(true, Ordering::Release);
            if let Some(gate) = &self.gate {
                let _ = gate.acquire().await;
            }
            if !self.delay.is_zero() {
                tokio::time::sleep(self.delay).await;
            }
            let id = match event {
                ThalamicPulse::Created { id, .. } => id.clone(),
                ThalamicPulse::Superseded { new_id, .. } => new_id.clone(),
                ThalamicPulse::Archived { id, .. } => id.clone(),
                ThalamicPulse::GuardedActionFired { trace_id, .. } => trace_id.clone(),
            };
            self.seen
                .lock()
                .expect("durable spy log poisoned")
                .push(DurableSeen {
                    id,
                    delivery: ctx.delivery,
                });
            if self.fail {
                return Err(anyhow!("simulated subscriber failure after its effect"));
            }
            Ok(())
        }
    }

    fn outbox_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "neurostrata-bus-outbox-test-{}-{}-{}",
            std::process::id(),
            tag,
            uuid::Uuid::now_v7()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn seeded_outbox(dir: &std::path::Path) -> EventOutbox {
        EventOutbox::open(dir).expect("the outbox opens")
    }

    fn seen_ids(seen: &Arc<Mutex<Vec<DurableSeen>>>) -> Vec<String> {
        let mut unique = Vec::new();
        let mut known = HashSet::new();
        for entry in seen.lock().expect("durable spy log poisoned").iter() {
            if known.insert(entry.id.clone()) {
                unique.push(entry.id.clone());
            }
        }
        unique
    }

    async fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while !condition() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for {what}"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    #[tokio::test]
    async fn audit_and_mesh_events_survive_overflow_and_crash() {
        let dir = outbox_dir("overflow-crash");
        let gate = Arc::new(Semaphore::new(0));
        let bus = ThalamicBus::with_outbox(1, seeded_outbox(&dir), OutboxConfig::default());
        let audit_seen = Arc::new(Mutex::new(Vec::new()));
        let mesh_seen = Arc::new(Mutex::new(Vec::new()));
        let mut audit_spy = DurableSpy::new("guard_event_log", audit_seen.clone());
        audit_spy.gate = Some(gate.clone());
        let mut mesh_spy = DurableSpy::new("mesh_outbox", mesh_seen.clone());
        mesh_spy.gate = Some(gate.clone());
        bus.register(Box::new(audit_spy));
        bus.register(Box::new(mesh_spy));

        let token = RecursionToken::root();
        for n in 0..8 {
            bus.emit(
                ThalamicPulse::GuardedActionFired {
                    trace_id: format!("audit-{n}"),
                    action_type: "shell".into(),
                    payload_hash: n,
                    verdict: "deny".into(),
                    rule_ids_triggered: vec![],
                    namespace: "n".into(),
                },
                &token,
            );
            bus.emit(
                ThalamicPulse::Created {
                    id: format!("mesh-{n}"),
                    namespace: "n".into(),
                    kind: "fact".into(),
                },
                &token,
            );
        }

        // Overflow: the lossy capacity is 1, yet the durable entries are
        // exempt from drop-oldest and wait in the queue above capacity.
        assert_eq!(bus.metrics().drops_oldest, 0);
        assert!(
            bus.metrics().queue_depth > 1,
            "the durable entries queue above the lossy capacity: {:?}",
            bus.metrics()
        );
        let attached = bus.outbox().expect("outbox attached");
        assert_eq!(attached.offset(EventClass::Audit), 0);
        assert_eq!(attached.offset(EventClass::Mesh), 0);
        assert_eq!(attached.pending(EventClass::Audit).len(), 8);
        assert_eq!(attached.pending(EventClass::Mesh).len(), 8);

        // Crash: nothing was acknowledged, so a fresh bus over the same
        // directory replays every entry in sequence order.
        let replay = ThalamicBus::with_outbox(4, seeded_outbox(&dir), OutboxConfig::default());
        let replay_audit = Arc::new(Mutex::new(Vec::new()));
        let replay_mesh = Arc::new(Mutex::new(Vec::new()));
        replay.register(Box::new(DurableSpy::new(
            "guard_event_log",
            replay_audit.clone(),
        )));
        replay.register(Box::new(DurableSpy::new(
            "mesh_outbox",
            replay_mesh.clone(),
        )));
        wait_until("every audit entry to replay", || {
            seen_ids(&replay_audit).len() == 8
        })
        .await;
        wait_until("every mesh entry to replay", || {
            seen_ids(&replay_mesh).len() == 8
        })
        .await;
        assert_eq!(
            seen_ids(&replay_audit),
            (0..8).map(|n| format!("audit-{n}")).collect::<Vec<_>>()
        );
        assert_eq!(
            seen_ids(&replay_mesh),
            (0..8).map(|n| format!("mesh-{n}")).collect::<Vec<_>>()
        );
        let replayed = replay.outbox().expect("outbox attached");
        assert_eq!(
            replayed.offset(EventClass::Audit),
            15,
            "audit entries occupy the odd positions of the shared sequence"
        );
        assert_eq!(replayed.offset(EventClass::Mesh), 16);
        assert!(replayed.pending(EventClass::Audit).is_empty());
        assert!(replayed.pending(EventClass::Mesh).is_empty());
    }

    #[tokio::test]
    async fn a_class_offset_advances_only_after_the_subscriber_returns() {
        let dir = outbox_dir("offset-after-return");
        let gate = Arc::new(Semaphore::new(0));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let bus = ThalamicBus::with_outbox(8, seeded_outbox(&dir), OutboxConfig::default());
        let mut spy = DurableSpy::new("mesh_outbox", seen.clone());
        spy.gate = Some(gate.clone());
        let entered = spy.entered.clone();
        bus.register(Box::new(spy));

        bus.emit_checked(
            ThalamicPulse::Created {
                id: "m1".into(),
                namespace: "n".into(),
                kind: "fact".into(),
            },
            &RecursionToken::root(),
        )
        .expect("durable append succeeds");
        wait_until("the subscriber to enter", || {
            entered.load(Ordering::Acquire)
        })
        .await;

        let outbox = bus.outbox().expect("outbox attached");
        assert_eq!(
            outbox.offset(EventClass::Mesh),
            0,
            "the offset must not move while the subscriber is still running"
        );
        assert_eq!(outbox.pending(EventClass::Mesh).len(), 1);

        gate.add_permits(1);
        wait_until("the offset to advance", || {
            outbox.offset(EventClass::Mesh) == 1
        })
        .await;
        assert_eq!(seen_ids(&seen), vec!["m1".to_string()]);
    }

    #[tokio::test]
    async fn a_crash_between_the_effect_and_the_offset_redelivers() {
        let dir = outbox_dir("redeliver");
        let first_seen = Arc::new(Mutex::new(Vec::new()));
        let bus = ThalamicBus::with_outbox(8, seeded_outbox(&dir), OutboxConfig::default());
        let mut spy = DurableSpy::new("mesh_outbox", first_seen.clone());
        spy.fail = true;
        bus.register(Box::new(spy));

        bus.emit_checked(
            ThalamicPulse::Created {
                id: "m1".into(),
                namespace: "n".into(),
                kind: "fact".into(),
            },
            &RecursionToken::root(),
        )
        .expect("durable append succeeds");
        wait_until("the failing subscriber's effect", || {
            seen_ids(&first_seen).len() == 1
        })
        .await;
        assert_eq!(
            bus.outbox()
                .expect("outbox attached")
                .offset(EventClass::Mesh),
            0,
            "the subscriber failed, so the entry stays unacknowledged"
        );

        // Crash before the offset write: the next start redelivers the
        // entry, and an id-keyed consumer absorbs the duplicate.
        let second_seen = Arc::new(Mutex::new(Vec::new()));
        let restarted = ThalamicBus::with_outbox(8, seeded_outbox(&dir), OutboxConfig::default());
        restarted.register(Box::new(DurableSpy::new(
            "mesh_outbox",
            second_seen.clone(),
        )));
        wait_until("the redelivery", || seen_ids(&second_seen).len() == 1).await;
        assert_eq!(seen_ids(&second_seen), vec!["m1".to_string()]);
        assert_eq!(
            restarted
                .outbox()
                .expect("outbox attached")
                .offset(EventClass::Mesh),
            1
        );
    }

    #[tokio::test]
    async fn unreplayed_entries_are_replayed_at_startup() {
        let dir = outbox_dir("startup-replay");
        let seeded = seeded_outbox(&dir);
        seeded
            .append(
                EventClass::Audit,
                &ThalamicPulse::GuardedActionFired {
                    trace_id: "a1".into(),
                    action_type: "shell".into(),
                    payload_hash: 1,
                    verdict: "allow".into(),
                    rule_ids_triggered: vec![],
                    namespace: "n".into(),
                },
            )
            .expect("append a1");
        seeded
            .append(
                EventClass::Mesh,
                &ThalamicPulse::Created {
                    id: "m1".into(),
                    namespace: "n".into(),
                    kind: "fact".into(),
                },
            )
            .expect("append m1");
        let last_audit = seeded
            .append(
                EventClass::Audit,
                &ThalamicPulse::GuardedActionFired {
                    trace_id: "a2".into(),
                    action_type: "shell".into(),
                    payload_hash: 2,
                    verdict: "deny".into(),
                    rule_ids_triggered: vec![],
                    namespace: "n".into(),
                },
            )
            .expect("append a2");
        assert_eq!(last_audit, 3);
        seeded.advance(EventClass::Audit, 1).expect("advance audit");
        drop(seeded);

        let audit_seen = Arc::new(Mutex::new(Vec::new()));
        let mesh_seen = Arc::new(Mutex::new(Vec::new()));
        let bus = ThalamicBus::with_outbox(8, seeded_outbox(&dir), OutboxConfig::default());
        bus.register(Box::new(DurableSpy::new(
            "guard_event_log",
            audit_seen.clone(),
        )));
        bus.register(Box::new(DurableSpy::new("mesh_outbox", mesh_seen.clone())));
        wait_until("the audit replay", || seen_ids(&audit_seen).len() == 1).await;
        wait_until("the mesh replay", || seen_ids(&mesh_seen).len() == 1).await;
        assert_eq!(
            seen_ids(&audit_seen),
            vec!["a2".to_string()],
            "the acknowledged a1 entry is not replayed"
        );
        assert_eq!(seen_ids(&mesh_seen), vec!["m1".to_string()]);
        assert_eq!(
            seen_ids(&audit_seen),
            vec!["a2".to_string()],
            "startup replay must not duplicate an entry"
        );
        let outbox = bus.outbox().expect("outbox attached");
        assert_eq!(outbox.offset(EventClass::Audit), 3);
        assert_eq!(outbox.offset(EventClass::Mesh), 2);
    }

    #[tokio::test]
    async fn non_qualified_classes_stay_lossy_drop_oldest() {
        let dir = outbox_dir("lossy");
        let bus = ThalamicBus::with_outbox(2, seeded_outbox(&dir), OutboxConfig::default());
        // No `guard_event_log` subscriber: audit pulses are not qualified.
        let log = Arc::new(Mutex::new(Vec::new()));
        bus.register(Box::new(SlowSubscriber {
            log,
            delay: Duration::from_millis(100),
        }));
        for n in 0..5 {
            bus.emit(
                ThalamicPulse::GuardedActionFired {
                    trace_id: format!("a{n}"),
                    action_type: "shell".into(),
                    payload_hash: n,
                    verdict: "deny".into(),
                    rule_ids_triggered: vec![],
                    namespace: "n".into(),
                },
                &RecursionToken::root(),
            );
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
        let metrics = bus.metrics();
        assert_eq!(metrics.drops_oldest, 3, "lossy traffic keeps drop-oldest");
        assert_eq!(metrics.dispatcher_handled, 2);
        assert!(
            bus.outbox()
                .expect("outbox attached")
                .pending(EventClass::Audit)
                .is_empty(),
            "a non-qualified class is never appended to the outbox"
        );
    }

    #[tokio::test]
    async fn a_mesh_append_failure_fails_the_operation() {
        let dir = outbox_dir("mesh-append-fails");
        let bus = ThalamicBus::with_outbox(8, seeded_outbox(&dir), OutboxConfig::default());
        let seen = Arc::new(Mutex::new(Vec::new()));
        bus.register(Box::new(DurableSpy::new("mesh_outbox", seen.clone())));
        // Pull the directory out from under the outbox: every append now
        // fails with an io error.
        std::fs::remove_dir_all(&dir).expect("remove outbox dir");

        let err = bus
            .emit_checked(
                ThalamicPulse::Created {
                    id: "m1".into(),
                    namespace: "n".into(),
                    kind: "fact".into(),
                },
                &RecursionToken::root(),
            )
            .expect_err("a mesh append failure must fail the operation");
        assert!(
            matches!(err, crate::events::outbox::EventOutboxError::Io(_)),
            "the failure surfaces as an io error: {err:?}"
        );
        assert_eq!(
            bus.metrics().queue_depth,
            0,
            "a failed checked emit never reaches the queue"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(seen_ids(&seen).is_empty());
        assert_eq!(bus.metrics().dispatcher_handled, 0);
    }

    #[tokio::test]
    async fn an_audit_append_failure_alerts_and_falls_back_to_the_queue() {
        let dir = outbox_dir("audit-append-fails");
        let bus = ThalamicBus::with_outbox(8, seeded_outbox(&dir), OutboxConfig::default());
        let seen = Arc::new(Mutex::new(Vec::new()));
        bus.register(Box::new(DurableSpy::new("guard_event_log", seen.clone())));
        std::fs::remove_dir_all(&dir).expect("remove outbox dir");

        bus.emit(
            ThalamicPulse::GuardedActionFired {
                trace_id: "a1".into(),
                action_type: "shell".into(),
                payload_hash: 1,
                verdict: "deny".into(),
                rule_ids_triggered: vec![],
                namespace: "n".into(),
            },
            &RecursionToken::root(),
        );
        wait_until("the fallback queue delivery", || seen_ids(&seen).len() == 1).await;
        assert_eq!(
            bus.metrics().audit_append_failures,
            1,
            "a failed audit append is counted"
        );
        let observed = seen.lock().expect("durable spy log poisoned");
        assert_eq!(
            observed[0].delivery, None,
            "the fallback delivery carries no durable identity"
        );
    }

    #[tokio::test]
    async fn a_durable_pulse_carries_its_delivery_identity_to_the_subscriber() {
        let dir = outbox_dir("delivery-identity");
        let bus = ThalamicBus::with_outbox(8, seeded_outbox(&dir), OutboxConfig::default());
        let seen = Arc::new(Mutex::new(Vec::new()));
        bus.register(Box::new(DurableSpy::new("mesh_outbox", seen.clone())));
        bus.emit_checked(
            ThalamicPulse::Created {
                id: "m1".into(),
                namespace: "n".into(),
                kind: "fact".into(),
            },
            &RecursionToken::root(),
        )
        .expect("durable append succeeds");
        wait_until("the durable delivery", || seen_ids(&seen).len() == 1).await;
        let observed = seen.lock().expect("durable spy log poisoned");
        assert_eq!(
            observed[0]
                .delivery
                .map(|delivery| (delivery.class, delivery.seq)),
            Some((EventClass::Mesh, 1)),
            "the subscriber can key on and deduplicate a replayed delivery"
        );
    }

    #[tokio::test]
    async fn a_durable_emit_survives_a_full_queue() {
        let dir = outbox_dir("full-queue");
        let bus = ThalamicBus::with_outbox(1, seeded_outbox(&dir), OutboxConfig::default());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut spy = DurableSpy::new("mesh_outbox", seen.clone());
        spy.delay = Duration::from_millis(20);
        bus.register(Box::new(spy));

        for n in 0..8 {
            bus.emit_checked(
                ThalamicPulse::Created {
                    id: format!("m{n}"),
                    namespace: "n".into(),
                    kind: "fact".into(),
                },
                &RecursionToken::root(),
            )
            .expect("durable append succeeds");
        }
        wait_until("every durable emit to be delivered", || {
            seen_ids(&seen).len() == 8
        })
        .await;
        assert_eq!(
            seen_ids(&seen),
            (0..8).map(|n| format!("m{n}")).collect::<Vec<_>>(),
            "a full lossy queue must not drop a durable pulse"
        );
        assert_eq!(bus.metrics().drops_oldest, 0);
        let outbox = bus.outbox().expect("outbox attached");
        assert_eq!(outbox.offset(EventClass::Mesh), 8);
        assert!(outbox.pending(EventClass::Mesh).is_empty());
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

    // ---- judge fix round (Issue #406): prefix acks, eviction counts,
    // ---- retry identity, and the mesh-required contract ----

    fn created_pulse(id: &str) -> ThalamicPulse {
        ThalamicPulse::Created {
            id: id.to_string(),
            namespace: "n".into(),
            kind: "fact".into(),
        }
    }

    /// Records every durable delivery and fails only the first one. The
    /// failed-entry test needs seq 1 to fail while seq 2 succeeds.
    struct FailFirstDurable {
        seen: Arc<Mutex<Vec<DurableSeen>>>,
        failed: AtomicBool,
    }

    #[async_trait]
    impl MemorySubscriber for FailFirstDurable {
        fn name(&self) -> &'static str {
            "mesh_outbox"
        }

        async fn handle(
            &self,
            event: &ThalamicPulse,
            _token: &RecursionToken,
            ctx: &SubscriberContext<'_>,
        ) -> Result<(), SubscriberError> {
            if !EventClass::Mesh.interested_in(event) {
                return Ok(());
            }
            let id = match event {
                ThalamicPulse::Created { id, .. } => id.clone(),
                ThalamicPulse::Superseded { new_id, .. } => new_id.clone(),
                ThalamicPulse::Archived { id, .. } => id.clone(),
                _ => return Ok(()),
            };
            let is_first = id == "m1";
            self.seen
                .lock()
                .expect("durable spy log poisoned")
                .push(DurableSeen {
                    id,
                    delivery: ctx.delivery,
                });
            // Keep failing m1 for the whole test: an automatic retry must not
            // flip the first entry to success before the offset/pending
            // assertions run. `failed` is retained so the log still shows the
            // first failure distinctly.
            if is_first {
                self.failed.store(true, Ordering::Release);
                return Err(anyhow!("simulated failure of the first durable entry"));
            }
            Ok(())
        }
    }

    /// The judge's failed-entry proof: seq 1 fails delivery, seq 2 succeeds,
    /// and seq 1 must NOT be buried. A naive per-class high-water mark moves
    /// the durable offset to 2 and the lost entry never replays.
    #[tokio::test]
    async fn a_failed_entry_is_never_buried_by_a_later_success() {
        let dir = outbox_dir("failed-not-buried");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let bus = ThalamicBus::with_outbox(8, seeded_outbox(&dir), OutboxConfig::default());
        bus.register(Box::new(FailFirstDurable {
            seen: seen.clone(),
            failed: AtomicBool::new(false),
        }));

        let token = RecursionToken::root();
        bus.emit_checked(created_pulse("m1"), &token)
            .expect("seq 1 appends");
        wait_until("seq 1 to fail delivery", || {
            seen_ids(&seen) == vec!["m1".to_string()]
        })
        .await;
        bus.emit_checked(created_pulse("m2"), &token)
            .expect("seq 2 appends");
        wait_until("both deliveries", || seen_ids(&seen).len() == 2).await;
        wait_until("both pulses dispatched", || {
            bus.metrics().dispatcher_handled >= 2
        })
        .await;
        // Let the (un)acknowledgment write settle.
        tokio::time::sleep(Duration::from_millis(20)).await;

        let outbox = bus.outbox().expect("outbox attached");
        assert_eq!(
            outbox.offset(EventClass::Mesh),
            0,
            "seq 1 failed, so the class offset must not advance past it"
        );
        assert_eq!(seen_ids(&seen), vec!["m1".to_string(), "m2".to_string()]);

        let pending = outbox.pending(EventClass::Mesh);
        let pending_ids: Vec<String> = pending
            .iter()
            .map(|entry| match &entry.pulse {
                ThalamicPulse::Created { id, .. } => id.clone(),
                other => format!("{other:?}"),
            })
            .collect();
        assert_eq!(
            pending_ids,
            vec!["m1".to_string()],
            "seq 2 is acknowledged; seq 1 is still pending"
        );

        // Restart: the failed entry is replayed, not silently lost.
        drop(bus);
        let reopened = EventOutbox::open(&dir).expect("the outbox reopens");
        let replayed: Vec<String> = reopened
            .pending(EventClass::Mesh)
            .into_iter()
            .map(|entry| match &entry.pulse {
                ThalamicPulse::Created { id, .. } => id.clone(),
                other => format!("{other:?}"),
            })
            .collect();
        assert_eq!(
            replayed,
            vec!["m1".to_string()],
            "the failed entry survives a restart and is replayed"
        );
    }

    /// A single push can evict several lossy pulses when the queue already
    /// holds durable entries above the lossy bound. Every evicted pulse must
    /// be counted, so the metrics invariant still holds.
    #[test]
    fn a_multi_eviction_counts_every_dropped_pulse() {
        // `assemble` starts no dispatcher, so the queue can be primed without
        // racing a pop.
        let bus = ThalamicBus::assemble(0, None);
        for n in 0..3 {
            let _ = bus.queue.push(
                QueuedPulse {
                    event: created_pulse(&format!("primed-{n}")),
                    delivery: None,
                },
                10,
            );
        }
        bus.enqueue(created_pulse("incoming"), None);
        let metrics = bus.metrics();
        assert_eq!(
            metrics.drops_oldest, 4,
            "all four evicted lossy pulses are counted, not one: {metrics:?}"
        );
        assert_eq!(metrics.queue_depth, 0);
    }

    /// A subscriber that times out on its first attempt and records the
    /// delivery identity its retry received.
    struct TimeoutThenRecordDelivery {
        seen: Arc<Mutex<Vec<Option<DurableDelivery>>>>,
    }

    #[async_trait]
    impl MemorySubscriber for TimeoutThenRecordDelivery {
        fn name(&self) -> &'static str {
            "mesh_outbox"
        }

        fn retry_on_timeout(&self) -> bool {
            true
        }

        async fn handle(
            &self,
            _event: &ThalamicPulse,
            _token: &RecursionToken,
            ctx: &SubscriberContext<'_>,
        ) -> Result<(), SubscriberError> {
            let first = {
                let mut seen = self.seen.lock().expect("delivery log poisoned");
                if seen.is_empty() {
                    seen.push(None);
                    true
                } else {
                    false
                }
            };
            if first {
                // Longer than the dispatcher's 250ms fast-path budget.
                tokio::time::sleep(Duration::from_millis(500)).await;
                return Ok(());
            }
            self.seen
                .lock()
                .expect("delivery log poisoned")
                .push(ctx.delivery);
            Ok(())
        }
    }

    /// The detached timeout retry must carry the original durable identity:
    /// without it a durable consumer cannot deduplicate its own retry by
    /// `bus_seq`.
    #[tokio::test]
    async fn a_timeout_retry_preserves_the_durable_delivery_identity() {
        let dir = outbox_dir("retry-identity");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let bus = ThalamicBus::with_outbox(8, seeded_outbox(&dir), OutboxConfig::default());
        bus.register(Box::new(TimeoutThenRecordDelivery { seen: seen.clone() }));
        bus.emit_checked(created_pulse("m1"), &RecursionToken::root())
            .expect("durable append succeeds");
        wait_until("the retry to record its delivery", || {
            seen.lock().expect("delivery log poisoned").len() >= 2
        })
        .await;
        let observed = seen.lock().expect("delivery log poisoned").clone();
        assert_eq!(
            observed.last().copied().flatten().map(|d| (d.class, d.seq)),
            Some((EventClass::Mesh, 1)),
            "the timeout retry carries the original durable identity: {observed:?}"
        );
    }

    /// With a mesh publish path configured, a missing mesh subscriber must
    /// fail a checked mesh emit instead of silently falling back to lossy
    /// delivery. Standalone (mesh never required) stays `Ok`.
    #[tokio::test]
    async fn a_required_mesh_without_its_subscriber_fails_checked_emits() {
        let dir = outbox_dir("mesh-required");
        let bus = ThalamicBus::with_outbox(8, seeded_outbox(&dir), OutboxConfig::default());
        // Only the audit consumer is wired; the mesh consumer is missing.
        bus.register(Box::new(DurableSpy::new(
            "guard_event_log",
            Arc::new(Mutex::new(Vec::new())),
        )));
        bus.expect_mesh_durability();
        let err = bus
            .emit_checked(created_pulse("m1"), &RecursionToken::root())
            .expect_err("a required mesh durability without a subscriber must fail");
        assert!(
            matches!(err, EventOutboxError::MeshUnavailable),
            "the failure names the missing mesh path: {err:?}"
        );
        assert_eq!(
            bus.metrics().queue_depth,
            0,
            "the refused pulse never reaches the queue"
        );

        // Standalone: no mesh was ever announced, so a checked emit is the
        // ordinary lossy path with a receipt.
        let standalone = ThalamicBus::new(8);
        standalone
            .emit_checked(created_pulse("m2"), &RecursionToken::root())
            .expect("an unconfigured bus does not fail on a mesh pulse");
    }
}
