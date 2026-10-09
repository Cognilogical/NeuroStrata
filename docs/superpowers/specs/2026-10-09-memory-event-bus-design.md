# Memory-Event Bus — Design Spec

**Date:** 2026-10-09
**Author:** BOSUN session
**Path:** architectural (per `superpowers:brainstorming` ratchet)
**Status:** design approved (conversational); pending user review of this written spec

---

## Intent

Wire storage mutations as a publisher. v1 has two specific jobs:

1. **Close the Continuous Backup Protocol gap.** Today `neurostrata_append_log` is called manually by the agent each turn. The episodic buffer (`.NeuroStrata/sessions/current.md` — the same file `buffer::append_entry` writes to) is the audit substrate the gate already consults, but writes are best-effort. The v1 pointer-echo subscriber turns this into automatic, fire-and-forget bookkeeping; the gate now sees the union of manual `append_log` calls and automatic echoes.
2. **Make the export-freshness gate (A4 wire) actually automatic.** A per-namespace `dirty: true` flag, set on any mutation, replaces the missing-freshness-marker failure mode the panel calls out (the gate-side consumer lands in a follow-up task; in 1.8.0 the SET side ships but the gate does not yet consult the flag).

A third v1 subscriber — `GuardEventLog` — records every `neurostrata_guard_validate` call. This unlocks the deferred `export-graph --causal` work as a 1-line projection over existing rows.

Success looks like: `add_memory` writes land in the episodic buffer without the agent calling `append_log`. The export-freshness gate's `dirty` flag is current without a separate probe *(the gate-side consumer is a follow-up task; 1.8.0 ships the SET side)*. A future contributor can register a new subscriber in 10 lines and it works.

## Non-goals (v1)

- No MCP-visible signal tools (per K3 verdict — server-internal only).
- No persistent event log / replay-on-restart. K3: "won't happen at agent-tool-call rates." Defer until proven otherwise.
- No subscriber priority / ordering. All subscribers are independent.
- No macro-based subscriber registration.
- No `PostCompact` hook surface (separate work, not in this design).

## Architecture

The bus is **explicit, owned, threaded**. It is not a global. Construction lives in `daemon.rs`; the resulting `Arc<MemoryEventBus>` is passed through `process_mcp_request` as a 5th parameter alongside `emb`, `store`, `ingests`, `deduplication_checker`.

```
              ┌────────────────────────────────────────┐
              │  daemon (daemon.rs)                    │
              │  Arc<MemoryEventBus> held as field     │
              └──────────────────┬─────────────────────┘
                                 │ thread through
                                 ▼
              ┌────────────────────────────────────────┐
              │  process_mcp_request(req, emb, store,  │
              │    ingests, bus)                       │
              └────┬───────┬───────┬───────┬───────────┘
                   │       │       │       │
                   ▼       ▼       ▼       ▼
            handle_add  handle_  handle_  handle_
            _memory    super-   task_    archive
                      sede     complete  _memory
                       │       │       │
                       └───────┴───────┘
                                 │ bus.emit(event, &RecursionToken::root())
                                 ▼
              ┌────────────────────────────────────────┐
              │  MemoryEventBus                        │
              │  • bounded queue (cap 10000)           │
              │  • single dispatcher task              │
              │  • Vec<Box<dyn MemorySubscriber>>     │
              │  • RecursionToken guard                │
              │  • Watch channel for BackpressureEvents│
              └──────────────────┬─────────────────────┘
                                 │ dispatch sequentially
                                 ▼
              EpisodicPointerEcho  ExportFreshnessDirty  GuardEventLog
```

A subscriber never holds a reference to the bus. It receives the event, the `RecursionToken`, and a context. If a subscriber needs to emit a follow-up event (rare), it accepts the token as an explicit capability.

## Components and contracts

### `MemoryEvent` (in `src/events.rs`)

```rust
#[non_exhaustive]
pub enum MemoryEvent {
    Created    { id: String, namespace: String, kind: String },
    Superseded { old_id: String, new_id: String, namespace: String },
    Archived   { id: String, namespace: String },
}
```

`#[non_exhaustive]` so future event kinds land without breaking downstream consumers.

### `MemoryEventBus` (in `src/events.rs`)

- `pub fn new(capacity: usize) -> Self` — constructs the bus, starts the dispatcher task on the current Tokio runtime.
- `pub fn attach_store(&self, store: Arc<dyn VectorStore>)` — hands the daemon's store to the (already running) dispatcher, which stamps it into every `SubscriberContext`. Until it is called the dispatcher substitutes an inert stand-in whose methods all return an explicit "no store attached" error, so pulses still flow and a subscriber reading `ctx.store` early fails loudly rather than writing nowhere. The first store wins; a second attach is a logged no-op.
- `pub fn register(&self, sub: Box<dyn MemorySubscriber>) -> SubscriberId` — appends to the subscriber list. Takes `&self`, not `&mut self`: the daemon shares one `Arc<ThalamicBus>` between its startup sequence and the request handlers, so registration cannot borrow the bus mutably. Returns an id for later introspection (`bus.metrics()`).
- `pub fn emit(&self, event: ThalamicPulse, token: &RecursionToken)` — non-blocking. Caller passes a token (emit sites use `&RecursionToken::root()` for depth 0; future subscriber-internal calls would pass a depth-1 token). Returns nothing; the storage write never waits for downstream. If the token's `allows_emit()` is false, the bus drops the event, increments `recursion_blocks`, and publishes `BackpressureEvent::RecursionRefused { token_allows_emit: false }` — the recursion guard is enforced at this boundary. The distinct variant keeps a recursion refusal distinguishable from a queue-full drop (`Dropped`) at `POST /bus/metrics`, without correlating either against a counter.
- `pub fn backpressure(&self) -> tokio::sync::watch::Receiver<BackpressureEvent>` — exposes the sidecar watch channel. `#[cfg(test)]` in 1.8.0: there is no production consumer yet (`POST /bus/metrics` reads `bus.metrics()`, not this channel); the accessor is kept for the tests that assert the seed and the published transitions. The channel is seeded with `BackpressureEvent::Idle`, so a consumer that reads before the bus publishes anything sees "nothing has happened yet" rather than a semantically false zero-count drop.
- `pub fn metrics(&self) -> BusMetrics` — `events_emitted: u64, dispatcher_handled: u64, subscribers: usize, queue_depth: usize, drops_oldest: u64, subscriber_panics: u64, subscriber_timeouts: u64, recursion_blocks: u64`. `events_emitted` counts every pulse the bus accepted into its queue (including the one that arrived when the queue was full and evicted the oldest); a refused recursion counts zero. `drops_oldest` counts only pulses that entered-or-should-have-entered and were lost to a full queue, `dispatcher_handled` counts pulses the dispatcher finished a full subscriber pass on. Invariant (holds whenever the dispatcher is idle between pulses, since a pulse mid-pass is in neither the queue nor the handled count yet): `events_emitted == dispatcher_handled + queue_depth + drops_oldest`.

Internals:
- A `VecDeque<MemoryEvent>` under a `std::sync::Mutex` with a `tokio::sync::Notify` as the wakeup edge (not an mpsc channel: with a channel the receiver lives in the dispatcher's task and is parked holding its guard across `recv().await` whenever the queue is empty — the common case — so the pop that "makes room" cannot take the lock, and drop-oldest silently degrades to drop-newest). `emit` pushes under the std lock, trims the oldest when the push overflows capacity, and returns — the lock is never held across an await. Overflow evicts the oldest, increments `drops_oldest`, publishes a `BackpressureEvent::Dropped { count }`; `Dropped` is queue-full only, recursion refusals publish `RecursionRefused` (see `emit` above). `Notify::notify_one` stores its permit when the dispatcher is parked-to-empty, so a push between the dispatcher's empty pop and its `notified().await` is never lost.
- Dispatcher task: `loop { let event = match queue.pop() { Some(e) => e, None => { queue.notified().await; continue } }; for sub in &self.subscribers { let token = RecursionToken::new(); let timeout = tokio::time::timeout(Duration::from_millis(250), sub.handle(&event, &token, &ctx)); match timeout.await { Ok(Ok(())) => {} Ok(Err(e)) => log subscriber error, increment metric, Ok(Err(panic)) => caught, log WARN with the full panic text, publish `SubscriberPanic` with a redacted snippet, increment metric, Err(_) => timeout, log, increment metric, } } dispatcher_handled += 1 }` — a pulse counts as handled after its full subscriber pass, whatever the individual outcomes were.
- Single dispatcher (not one-task-per-subscriber) for ordering guarantees and backpressure simplicity.

### `MemorySubscriber` trait (in `src/events.rs`)

```rust
#[async_trait]
pub trait MemorySubscriber: Send + Sync {
    fn name(&self) -> &'static str;
    async fn handle(&self, event: &MemoryEvent, token: &RecursionToken, ctx: &SubscriberContext)
        -> Result<(), SubscriberError>;
}
```

Subscribers opt in by `match` on `event`. Unhandled variants are no-ops (zero-cost).

### `RecursionToken` (in `src/events.rs`)

- `pub fn allows_emit(&self) -> bool` — false when the dispatcher is inside a subscriber invocation (depth ≥ 1).
- `pub fn upgrade(&self) -> Option<RecursionToken>` — returns a token that allows emit, or `None` if already at depth ≥ N (default N=1).
- `upgrade` mutates the caller's token (the deeper slot is reserved on the parent's counter); there is no decrement path. Panic safety does not need one: `upgrade` returns an owned token, so dropping it discards that depth with nothing to release.

### `SubscriberContext` (in `src/events.rs`)

- `&dyn VectorStore` — the store handle a subscriber uses to persist or read rows. The v1 `ExportFreshnessDirty` subscriber uses it only to `upsert` its dirty sentinel; it reads no sibling rows and computes no hash (no such consumer exists in 1.8.0).
- `&str` namespace + ts already on the event.

### v1 subscribers (in `src/events/subscribers/`)

1. **`EpisodicPointerEcho`** — matches `Created`. Writes one line of the form `{POINTER_LABEL}\t{id}\t{kind}\t{namespace}` to the per-project episodic buffer at `<project_root>/.NeuroStrata/sessions/current.md` — the SAME file the existing `buffer::append_entry` writes to, so manual `append_log` calls and automatic echoes land in one place. The v1 path deliberately does not roll the file by date: the union requirement (manual + automatic in one file) plus the existing buffer module's `current.md` contract both pin this. `POINTER_LABEL` is the literal string the subscriber prepends so its own lines are distinguishable from free-text agent notes; the idempotency check matches on `(id, kind)` (fields 1–2 of the tab-separated record). **Honors `buffer::load_config().enabled`** — if an operator sets `episodic_buffer: false` in their config, the subscriber returns `Ok(())` immediately without writing.

2. **`ExportFreshnessDirty`** — matches `Created`, `Superseded`, `Archived`. Sets a `dirty: true` flag in the per-namespace metadata row. The flag is set by every storage mutation; its consumer (export-freshness gate, A4 wire, per the wiring panel) and its clear path are deferred to a follow-up task — the gate does **not** consult this flag in 1.8.0. The flag is cleared on a successful `export-graph` run (new emit event in v2: `Exported { namespace }` — Task 7 wires the clear path; Task 4 ships only the SET side). The sentinel row's `memory_type` is `"freshness_flag"`, which is in `STRUCTURAL_MEMORY_TYPES` (`src/store/ladybug.rs:648` — the const now lists `["directory", "file", "markdown", "freshness_flag"]`) so vector `search_memory` excludes it from user-facing results. The sentinel is namespaced (`id = "export_freshness::{namespace}"`) so two namespaces cannot share one flag; `upsert` MERGEs on id alone, so a second flip updates the first row in place. **The subscriber takes `dimensions` at construction** (the store's embedding width — the sentinel is a row like any other, `Memory.embedding` is a fixed-size `FLOAT[N]`); the DAEMON (Task 8) passes `store.dimensions()` at construction, otherwise the write is silently rejected.

3. **`GuardEventLog`** — listens on a new emit site at the end of `handle_guard_validate` (see Emit Sites). Persists `{trace_id, action_type, payload_hash, verdict, rule_ids_triggered, ts, namespace}` as a `memory_type: "guard_event"` row. The deferred `export-graph --causal` becomes a 1-line filter over these rows. `payload_hash` is a 32-bit hash of the payload bytes (not the payload itself) — keeps the row small and avoids leaking secrets into a system of record.

## Emit sites (4)

1. `handle_add_memory` (server.rs) — after the schema-ready write succeeds, `bus.emit(Created { id, namespace, kind: memory_type })`. Failure of the bus does NOT roll back the write.
2. `handle_supersede_memory` (server.rs) — after the supersede write succeeds, `bus.emit(Superseded { old_id, new_id, namespace })`.
3. `handle_task_complete` (src/task/mod.rs) — in the inline extraction path (when the new memory is created as the EXTRACTED_FROM target of a closing task), `bus.emit(Superseded { old_id: task_id, new_id: lesson_id, namespace })`. The semantics: "the task gave way to a lesson." Reusing `Superseded` keeps the event count at 3; a future `Extracted` event can specialize.
4. New `handle_archive_memory` (server.rs) — a 6-line POST /memory/archive handler. Producers mark a memory as archived (set `metadata.archived: true` and write a tombstone). After the write succeeds, `bus.emit(Archived { id, namespace })`. Without this site, the `Archived` variant would be dead code in v1.

Each emit is wrapped in `if let Ok(()) = bus.emit(...).or_log()` — the bus is fire-and-forget; the caller does not inspect the result.

## Data flow

A `handle_add_memory` call:

1. Validate inputs, call `store.add(...)` — LadybugDB write.
2. `bus.emit(MemoryEvent::Created { ... })` — enqueue, return immediately.
3. Handler returns success to MCP caller.
4. Dispatcher task receives the event, dispatches to all 3 v1 subscribers.
5. Each subscriber runs with 250ms timeout; panic caught; result logged.
6. If queue was at capacity before the emit, oldest event was dropped; a `BackpressureEvent::Dropped` was published on the watch channel; the daemon exposed it via `POST /bus/metrics` and the next call returns the updated count.

## Error model

| Failure | Behavior |
|---|---|
| Subscriber panic | Caught at the dispatcher join point, logged WARN with the full panic payload (logs are local), `subscriber_panics` incremented, `BackpressureEvent::SubscriberPanic` published on the watch channel with a **redacted snippet** of the payload — first line only, control characters stripped, capped at 200 chars — because that channel is the audit path `POST /bus/metrics` surfaces and panic text is attacker- or content-influenced (the same reasoning as `GuardEventLog`'s `payload_hash`). Daemon continues. |
| Subscriber timeout (>250ms) | Logged WARN, `subscriber_timeouts` incremented, daemon continues. Tunable per-subscriber in v2 if needed. |
| Bus queue full (10000) | Drop-oldest, `drops_oldest` incremented, `BackpressureEvent::Dropped` published. WARN log at the rate of 1 per 1000 drops to avoid log flooding. |
| `emit` called under a held `RecursionToken` | Pulse dropped at the bus boundary before it reaches the queue, `recursion_blocks` incremented, `BackpressureEvent::RecursionRefused` published. The caller's storage write is untouched. |
| Bus emit called outside Tokio runtime | Defensive guard returns silently. Should never happen in production — daemon constructs the bus on a runtime. |
| `handle_archive_memory` HTTP endpoint hit by an agent | Bus emits `Archived`; subscribers may not handle it; that's fine. |

The bus is **never the cause of a failed write**. A subscriber crash does not propagate to the caller. The bus is observability + automation, not a transactional contract.

## Testing

### Unit tests
- Per subscriber, in isolation: feed 3 events (Created, Superseded, Archived), assert side effects.
- `MemoryEventBus::new(10)` then `register(noop_subscriber)`, emit 5 events, assert dispatcher ran them all in order.
- `MemoryEventBus::new(0)` (zero capacity — degenerate but valid): emit 1 event, assert drop + backpressure event.

### Integration tests
- All 3 v1 subscribers in one bus, drive 100 synthetic events of mixed kinds, assert each subscriber's recorder received the right slice (e.g., EpisodicPointerEcho only on Created).
- Recursion guard drill: a subscriber that, when given the token, attempts `bus.emit(...)` — assert the emit is silently dropped and the metric `recursion_blocks` is incremented.
- Backpressure drill: emit `capacity + 50` events with a subscriber that holds a 1s sleep; assert drops are reported and the daemon continues accepting new events.

### End-to-end
- Through the live MCP proxy: call `neurostrata_add_memory` with a fresh namespace, then read the episodic buffer file and the export-freshness flag, assert both updated.
- Through the live MCP proxy: call `neurostrata_guard_validate` with a benign payload, then read a guard_event row, assert the trace_id matches.
- Restart the daemon: confirm the bus re-initializes (the in-memory subscribers are reconstructed by the daemon at startup; no persistence — by design, v1).

## Extensibility surface

The system gets more powerful from this design without further architectural work:

- **New event kinds:** add a variant to `MemoryEvent` (non-breaking due to `#[non_exhaustive]`), update emit sites, subscribers opt in via match arms.
- **New subscribers:** implement `MemorySubscriber`, register at daemon construction. ~10 lines of glue.
- **Multiple buses:** `MemoryEventBus` is `Clone` (via `Arc`); the daemon can hold several and route by namespace.
- **Per-subscriber backpressure policy:** trait extension; default is drop-oldest, future variants can be drop-newest, block-with-deadline.
- **MCP-visible signal tools (deferred v2):** `neurostrata_signal_list` would call `bus.metrics()`; the surface is already plumbed, just not yet exposed.
- **Persistent event log (deferred):** a `PersistSubscriber` would write to disk; replay-on-restart would add a `ReplayedFromExport` event.

## Open questions

None at design approval. The recursion-token depth limit (default 1) is a single source of magic; if a future subscriber needs depth 2, that's a future change.

## What this design does NOT do

- Does not replace `neurostrata_append_log` for agent-authored content. The episodic buffer is the union of (manual `append_log` calls) and (automatic pointer echos). Agents that want to write a longer note still call `append_log` directly.
- Does not introduce a new event log persistence layer. If the daemon crashes between an emit and a subscriber's side effect, the side effect is lost. Acceptable for v1 — the Continuous Backup Protocol is about *observability*, not transactional durability.
- Does not change the export-graph pipeline. `export-graph --causal` (when added in v2) is a filter over existing guard_event rows; no new export format.
- Does not add subscriber observability to the gate. The `task gate --self-test` stays focused on the gate engine; bus metrics are a separate `POST /bus/metrics` endpoint.
