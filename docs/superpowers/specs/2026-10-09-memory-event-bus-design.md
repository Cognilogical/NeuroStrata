# Memory-Event Bus — Design Spec

**Date:** 2026-10-09
**Author:** BOSUN session
**Path:** architectural (per `superpowers:brainstorming` ratchet)
**Status:** design approved (conversational); pending user review of this written spec

---

## Intent

Wire storage mutations as a publisher. v1 has two specific jobs:

1. **Close the Continuous Backup Protocol gap.** Today `neurostrata_append_log` is called manually by the agent each turn. The episodic buffer (`.NeuroStrata/sessions/<date>.log`) is the audit substrate the gate already consults, but writes are best-effort. The v1 pointer-echo subscriber turns this into automatic, fire-and-forget bookkeeping.
2. **Make the export-freshness gate (A4 wire) actually automatic.** A per-namespace `dirty: true` flag, set on any mutation, replaces the missing-freshness-marker failure mode the panel calls out.

A third v1 subscriber — `GuardEventLog` — records every `neurostrata_guard_validate` call. This unlocks the deferred `export-graph --causal` work as a 1-line projection over existing rows.

Success looks like: `add_memory` writes land in the episodic buffer without the agent calling `append_log`. The export-freshness gate's `dirty` flag is current without a separate probe. A future contributor can register a new subscriber in 10 lines and it works.

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
                                 │ bus.emit(event)
                                 ▼
              ┌────────────────────────────────────────┐
              │  MemoryEventBus                        │
              │  • bounded mpsc (cap 10000)            │
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
    Created    { id: String, namespace: String, kind: &'static str },
    Superseded { old_id: String, new_id: String, namespace: String },
    Archived   { id: String, namespace: String },
}
```

`#[non_exhaustive]` so future event kinds land without breaking downstream consumers.

### `MemoryEventBus` (in `src/events.rs`)

- `pub fn new(capacity: usize) -> Self` — constructs the bus, starts the dispatcher task on the current Tokio runtime.
- `pub fn register(&mut self, sub: Box<dyn MemorySubscriber>) -> SubscriberId` — appends to the subscriber list. Returns an id for later introspection (`bus.metrics()`).
- `pub fn emit(&self, event: MemoryEvent)` — non-blocking. Returns nothing; the storage write never waits for downstream.
- `pub fn backpressure(&self) -> tokio::sync::watch::Receiver<BackpressureEvent>` — exposes the sidecar watch channel for `POST /bus/metrics`.
- `pub fn metrics(&self) -> BusMetrics` — `events_emitted: u64, subscribers: usize, queue_depth: usize, drops_oldest: u64, subscriber_panics: u64, subscriber_timeouts: u64`.

Internals:
- `tokio::sync::mpsc::Sender<MemoryEvent>` with bounded capacity (default 10000). `try_send` — when full, drop the oldest via `try_recv` + `try_send` cycle, increment `drops_oldest`, publish a `BackpressureEvent::Dropped { count }`.
- Dispatcher task: `while let Some(event) = rx.recv().await { for sub in &self.subscribers { let token = RecursionToken::new(); let timeout = tokio::time::timeout(Duration::from_millis(250), sub.handle(&event, &token, &ctx)); match timeout.await { Ok(Ok(())) => {} Ok(Err(e)) => log subscriber error, increment metric, Ok(Err(panic)) => caught, log, increment metric, Err(_) => timeout, log, increment metric, } } }`
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
- Bus guarantees the depth counter is decremented even on panic.

### `SubscriberContext` (in `src/events.rs`)

- `&dyn VectorStore` — for the subscriber that needs to read siblings (e.g., the freshness dirty subscriber computes a hash from the affected namespace).
- `&str` namespace + ts already on the event.

### v1 subscribers (in `src/events/subscribers/`)

1. **`EpisodicPointerEcho`** — matches `Created`. Writes one line `<ts> <namespace> <kind> <id>` to the per-project episodic buffer (path resolution: `<project_root>/.NeuroStrata/sessions/<YYYY-MM-DD>.log`). Idempotency key: `(id, kind)`; second emit for the same pair is a no-op. Skips if the agent has already appended a manual `append_log` entry for the same id within the same session (detected via a short-tail line-shape check, not full dedup — pragmatic, not perfect).

2. **`ExportFreshnessDirty`** — matches `Created`, `Superseded`, `Archived`. Sets a `dirty: true` flag in the per-namespace metadata row. The export-freshness gate (A4 wire, per the wiring panel) consults this flag instead of the mtime probe. The flag is cleared on a successful `export-graph` run (new emit event in v2: `Exported { namespace }`).

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
| Subscriber panic | Caught at the dispatcher join point, logged WARN with the panic payload, `subscriber_panics` incremented, daemon continues. |
| Subscriber timeout (>250ms) | Logged WARN, `subscriber_timeouts` incremented, daemon continues. Tunable per-subscriber in v2 if needed. |
| Bus queue full (10000) | Drop-oldest, `drops_oldest` incremented, `BackpressureEvent::Dropped` published. WARN log at the rate of 1 per 1000 drops to avoid log flooding. |
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
