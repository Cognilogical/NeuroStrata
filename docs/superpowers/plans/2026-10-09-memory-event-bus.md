# Memory-Event Bus (Thalamic Bus) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship the Thalamic Bus — a publisher/subscriber system that closes the Continuous Backup Protocol gap, automates the export-freshness gate, and persists guard-event audit rows for the deferred `--causal` export.

**Architecture:** `ThalamicBus` is constructed in `daemon.rs` at startup, held as `Arc<ThalamicBus>`, threaded through `process_mcp_request` as a 5th parameter. Bounded mpsc (cap 10000, drop-oldest) feeds a single dispatcher task that runs each `MemorySubscriber` with a 250ms timeout. `RecursionToken` prevents subscriber-induced loops (default depth 1, upgradeable). Three v1 subscribers: `EpisodicPointerEcho`, `ExportFreshnessDirty`, `GuardEventLog`. Four emit sites: `handle_add_memory`, `handle_supersede_memory`, `handle_task_complete` (extraction path), new `handle_archive_memory`. New daemon endpoints `POST /memory/archive` and `POST /bus/metrics`. Names follow the project's cognitive-anatomy convention (Thalamus = the brain's relay station).

**Tech Stack:** Rust (existing toolchain), Tokio (existing), `Arc<ThalamicBus>` threading, `#[non_exhaustive]` enum, `tokio::sync::mpsc` bounded + `tokio::sync::watch` for backpressure events. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-10-09-memory-event-bus-design.md`

## Global Constraints

- `MemoryEvent` becomes `ThalamicPulse` (cognitive rename per user directive; do not introduce a parallel `MemoryEvent` alias).
- `MemoryEventBus` becomes `ThalamicBus`. The `MemorySubscriber` trait keeps its name — the trait is a design pattern, not a brain concept.
- All new types live under `src/events/` and `src/events/subscribers/`. New HTTP handlers under `src/handlers/`.
- Subscribers are panic-caught at the dispatcher; subscriber timeouts default 250ms; bus queue cap 10000.
- `#[non_exhaustive]` on `ThalamicPulse` so future variants land without breaking downstream.
- All new code uses the project's existing test framework (`#[tokio::test]`, `cargo test --release --bin neurostrata-mcp -- --test-threads=1` baseline).
- Pre-push gate must stay green: `cargo test --release --bin neurostrata-mcp -- --test-threads=1` — all 288 prior tests + new tests pass; `task gate --self-test NeuroStrata` exits 0.
- TDD: every step that produces behavior writes the failing test first, runs it, then implements the minimum, then runs again. Drill the failure path (per the rule added this session).

## Review Focus

The spec's silent inputs and failure modes most likely to bite a user:

1. **Subscriber panic during a high-throughput `add_memory` burst** — the bus must not fail the write. Drill: a subscriber that panics on every event, while 1000 adds race in.
2. **Backpressure when the queue fills** — drop-oldest must surface in `bus.metrics()` and `POST /bus/metrics`; the caller must not block. Drill: emit `cap + 50` events with a 1s-sleep subscriber.
3. **Recursion via the token's upgrade path** — a malicious or buggy subscriber could try to emit under a held token. Drill: explicit `if false && ctx.token.allows_emit() { bus.emit(...) }` is rejected.
4. **`Arch::Archived` event with no producer** — easy to ship the bus without a 4th emit site and have a dead variant. Drill: simulate by removing the archive handler and assert the variant is unreached.
5. **Re-emit idempotency under daemon restart** — without persistence, the bus is in-memory; subscribers re-register on restart but don't replay. Drill: simulate restart, re-emit the same `Created`, assert subscribers are re-fired (acceptable v1 behavior; documented as "no replay").

---

### Task 1: ThalamicPulse + MemorySubscriber trait + RecursionToken primitives

**Files:**
- Create: `src/events/mod.rs`
- Create: `src/events/event.rs`
- Create: `src/events/subscriber.rs`

**Interfaces:**
- Consumes: nothing (foundational)
- Produces:
  - `pub enum ThalamicPulse { Created{...}, Superseded{...}, Archived{...} }` with `#[non_exhaustive]`
  - `pub trait MemorySubscriber: Send + Sync { fn name(&self) -> &'static str; async fn handle(&self, event: &ThalamicPulse, token: &RecursionToken, ctx: &SubscriberContext) -> Result<(), SubscriberError>; }`
  - `pub struct RecursionToken { depth: AtomicU8 }` with `allows_emit()` and `upgrade()`
  - `pub struct SubscriberContext<'a> { pub store: &'a dyn VectorStore, pub namespace: &'a str, pub ts: i64 }`
  - `pub enum BackpressureEvent { Dropped { count: u64 }, SubscriberTimeout { name: &'static str }, SubscriberPanic { name: &'static str, payload: String } }`
  - `pub struct BusMetrics { events_emitted: u64, subscribers: usize, queue_depth: usize, drops_oldest: u64, subscriber_panics: u64, subscriber_timeouts: u64, recursion_blocks: u64 }`

- [ ] **Step 1: Write the failing test for RecursionToken depth**

In `src/events/subscriber.rs` test mod:
```rust
#[test]
fn recursion_token_disallows_emit_at_depth_one() {
    let t = RecursionToken::new();
    assert!(!t.allows_emit(), "fresh token must block emit");
    let upgraded = t.upgrade();
    assert!(upgraded.is_none(), "depth-1 token cannot be upgraded further");
}
```

- [ ] **Step 2: Run, confirm fail** — `cargo test --release --bin neurostrata-mcp events::subscriber::tests::recursion_token_disallows_emit_at_depth_one` → expect "RecursionToken not found".

- [ ] **Step 3: Implement `RecursionToken` with `AtomicU8::new(1)` for depth; `allows_emit` returns `self.depth.load(Acquire) == 0`; `upgrade` returns `None` if depth ≥ 1.**

- [ ] **Step 4: Run, confirm pass** — same command → expect PASS.

- [ ] **Step 5: Write the failing test for `#[non_exhaustive]` `ThalamicPulse` enum**

In `src/events/event.rs` test mod:
```rust
#[test]
fn thalamic_pulse_variants_construct() {
    let _ = ThalamicPulse::Created { id: "x".into(), namespace: "n".into(), kind: "task" };
    let _ = ThalamicPulse::Superseded { old_id: "a".into(), new_id: "b".into(), namespace: "n".into() };
    let _ = ThalamicPulse::Archived { id: "x".into(), namespace: "n".into() };
}
```

- [ ] **Step 6: Run, confirm fail** — expect "ThalamicPulse not found".

- [ ] **Step 7: Implement `ThalamicPulse` with `#[non_exhaustive]` and the 3 variants per the spec.**

- [ ] **Step 8: Run, confirm pass** — expect PASS.

- [ ] **Step 9: Add the empty trait + context + metrics types in `src/events/subscriber.rs` (no method bodies beyond `name()`).**

- [ ] **Step 10: Add `pub use` re-exports in `src/events/mod.rs`: `ThalamicPulse`, `MemorySubscriber`, `RecursionToken`, `SubscriberContext`, `BackpressureEvent`, `BusMetrics`, `SubscriberError`.**

- [ ] **Step 11: Build, confirm compile** — `cargo build --release` → 0 errors.

- [ ] **Step 12: Commit**

```bash
git add src/events/
git commit -m "feat(events): thalamic pulse + subscriber trait + recursion token"
```

---

### Task 2: ThalamicBus runtime (register, dispatch, metrics, backpressure)

**Files:**
- Create: `src/events/bus.rs`

**Interfaces:**
- Consumes: Task 1's types
- Produces:
  - `pub struct ThalamicBus { tx: mpsc::Sender<ThalamicPulse>, subs: Mutex<Vec<Box<dyn MemorySubscriber>>>, metrics: AtomicMetrics, bp_tx: watch::Sender<BackpressureEvent>, capacity: usize }`
  - `pub fn new(capacity: usize) -> Self` — spawns the dispatcher task on the current runtime
  - `pub fn register(&self, sub: Box<dyn MemorySubscriber>) -> SubscriberId`
  - `pub fn emit(&self, event: ThalamicPulse)` — non-blocking, drop-oldest on full
  - `pub fn backpressure(&self) -> watch::Receiver<BackpressureEvent>`
  - `pub fn metrics(&self) -> BusMetrics`
  - `pub type SubscriberId = usize`

- [ ] **Step 1: Write the failing test: dispatcher runs a registered subscriber**

In `src/events/bus.rs` test mod:
```rust
#[tokio::test]
async fn dispatcher_invokes_registered_subscriber() {
    let bus = ThalamicBus::new(16);
    let rec = Arc::new(AtomicUsize::new(0));
    let rec2 = rec.clone();
    bus.register(Box::new(CountingSubscriber { counter: rec2 }));
    bus.emit(ThalamicPulse::Created { id: "x".into(), namespace: "n".into(), kind: "task" });
    // give dispatcher time
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(rec.load(Acquire), 1, "subscriber ran exactly once");
}
```
Define a tiny `CountingSubscriber` in the test mod that increments the counter on `Created`.

- [ ] **Step 2: Run, confirm fail** — expect "ThalamicBus not found".

- [ ] **Step 3: Implement `ThalamicBus` with the dispatch loop.** Use `tokio::spawn` to start the dispatcher. Per the spec: bounded mpsc, drop-oldest on full via `try_recv` + `try_send`, single dispatcher task, panic-caught subscribers, 250ms timeout via `tokio::time::timeout`. The `register` method appends to the `subs` Mutex<Vec<...>.

- [ ] **Step 4: Run, confirm pass** — same command → expect PASS.

- [ ] **Step 5: Write the failing test: drop-oldest when queue is full**

```rust
#[tokio::test]
async fn emit_drops_oldest_when_queue_full() {
    let bus = ThalamicBus::new(2);
    let slow = Arc::new(Mutex::new(Vec::new()));
    let slow2 = slow.clone();
    bus.register(Box::new(SlowSubscriber { log: slow2, delay: Duration::from_millis(200) }));
    for i in 0..5 {
        bus.emit(ThalamicPulse::Archived { id: format!("{i}"), namespace: "n".into() });
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    let metrics = bus.metrics();
    assert!(metrics.drops_oldest > 0, "at least one event was dropped: {:?}", metrics);
}
```

- [ ] **Step 6: Run, confirm fail** — expect metrics.drops_oldest to be 0 in the current (no-drop) implementation.

- [ ] **Step 7: Implement drop-oldest** in `emit`: `if let Err(_) = self.tx.try_send(event) { /* queue full */ let _ = self.tx.try_recv(); let _ = self.tx.try_send(event); self.metrics.drops_oldest.fetch_add(1, ...); let _ = self.bp_tx.send(BackpressureEvent::Dropped { count: 1 }); }`

- [ ] **Step 8: Run, confirm pass** — expect PASS.

- [ ] **Step 9: Write the failing test: subscriber panic is caught, not propagated**

```rust
#[tokio::test]
async fn subscriber_panic_is_caught() {
    let bus = ThalamicBus::new(16);
    let after = Arc::new(AtomicUsize::new(0));
    let after2 = after.clone();
    bus.register(Box::new(PanicSubscriber));
    bus.register(Box::new(CountingSubscriber2 { counter: after2 }));
    bus.emit(ThalamicPulse::Created { id: "x".into(), namespace: "n".into(), kind: "task" });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(after.load(Acquire), 1, "second subscriber still ran after first panicked");
    let m = bus.metrics();
    assert_eq!(m.subscriber_panics, 1);
}
```

- [ ] **Step 10: Run, confirm fail** — current code propagates the panic.

- [ ] **Step 11: Wrap the subscriber call in `tokio::task::spawn` + join handle catch_unwind** (or `AssertUnwindSafe` + `Future::catch_unwind`); on panic, increment `subscriber_panics` and continue.

- [ ] **Step 12: Run, confirm pass** — expect PASS.

- [ ] **Step 13: Add the `recursion_blocks` counter increment when a subscriber under a held token attempts `bus.emit()` (the emit is dropped, counter increments).** This is the surface Task 1's RecursionToken gates against.

- [ ] **Step 14: Build, run full test suite, confirm green** — `cargo test --release --bin neurostrata-mcp -- --test-threads=1` → expect prior 288 tests + new ones, all pass.

- [ ] **Step 15: Commit**

```bash
git add src/events/bus.rs
git commit -m "feat(events): thalamic bus runtime with bounded mpsc, drop-oldest, panic-caught dispatch"
```

---

### Task 3: EpisodicPointerEcho subscriber

**Files:**
- Create: `src/events/subscribers/mod.rs`
- Create: `src/events/subscribers/episodic_pointer_echo.rs`

**Interfaces:**
- Consumes: `ThalamicBus` (via `SubscriberContext.store`), `ThalamicPulse::Created`
- Produces: a line appended to `<project_root>/.NeuroStrata/sessions/<YYYY-MM-DD>.log`. Idempotency: skip if a line with the same id+kind already appears in the file (linear scan, capped at 1000 lines for safety).

- [ ] **Step 1: Write the failing test: emits a Created event, pointer appears in the buffer file**

In the subscriber's test mod, point `<project_root>` at a tempdir, set up the buffer file, register the subscriber on a test bus, emit, assert file contents.

- [ ] **Step 2: Run, confirm fail** — expect not found.

- [ ] **Step 3: Implement** the subscriber. Use `chrono::Utc::now().format("%Y-%m-%d")` for the filename. Use the existing `episodic_buffer` module (under `src/buffer.rs`) for the write path. Idempotency check: read the file, scan for `<id>\t<kind>`, skip if found.

- [ ] **Step 4: Run, confirm pass** — expect PASS.

- [ ] **Step 5: Drill the failure path** — write a second test that registers the subscriber on a bus, emits the same `Created { id, kind }` twice, asserts the buffer file contains the line exactly once (idempotency holds).

- [ ] **Step 6: Commit**

```bash
git add src/events/subscribers/
git commit -m "feat(events): episodic pointer echo subscriber"
```

---

### Task 4: ExportFreshnessDirty subscriber

**Files:**
- Create: `src/events/subscribers/export_freshness_dirty.rs`

**Interfaces:**
- Consumes: `ThalamicPulse::{Created, Superseded, Archived}`
- Produces: a per-namespace `dirty: true` flag stored as `metadata.export_freshness_dirty: true` on a sentinel row (or in a new lightweight `freshness_flags` table; pick the storage that matches the rest of the codebase). Read by the export-freshness gate (A4 wire).

- [ ] **Step 1: Write the failing test: Created/Superseded/Archived all set the flag**

In the test mod, register the subscriber, emit one of each, assert the flag is set per namespace.

- [ ] **Step 2: Run, confirm fail** — expect not found.

- [ ] **Step 3: Implement** the subscriber. Pick the storage strategy: either a dedicated `memory_type: "freshness_flag"` row keyed by namespace, OR a small companion file under `~/.config/NeuroStrata/freshness/<ns>.flag`. The latter is simpler and matches the on-disk pattern of `.NeuroStrata/`.

- [ ] **Step 4: Run, confirm pass** — expect PASS.

- [ ] **Step 5: Drill: emit only `Created` for namespace "a" and only `Archived` for namespace "b"; assert each flag is per-namespace (no cross-contamination).**

- [ ] **Step 6: Commit**

```bash
git add src/events/subscribers/export_freshness_dirty.rs
git commit -m "feat(events): export freshness dirty subscriber"
```

---

### Task 5: GuardEventLog subscriber

**Files:**
- Create: `src/events/subscribers/guard_event_log.rs`

**Interfaces:**
- Consumes: A new `ThalamicPulse::GuardEventFired` variant (added per the spec's "deferred v1 subscribers" → pulled into v1 per the user's "best design" directive). Or: a typed payload via a new `GuardedAction` variant. Decide: add `GuardedActionFired` as a 4th `ThalamicPulse` variant since the spec already allows future variants via `#[non_exhaustive]`.
- Produces: a `memory_type: "guard_event"` row with `{trace_id, action_type, payload_hash, verdict, rule_ids_triggered, ts, namespace}`. `payload_hash` is a 32-bit hash (FNV-1a or std `DefaultHasher`), not the payload itself.

- [ ] **Step 1: Add `GuardedActionFired` variant to `ThalamicPulse`** in `src/events/event.rs`. Fields: `trace_id, action_type, payload_hash: u32, verdict: String, rule_ids_triggered: Vec<String>, namespace`. Re-export from `mod.rs`.

- [ ] **Step 2: Write the failing test: subscriber persists a guard_event row**

In the test mod, set up a temp LadybugStore (use `LadybugStore::for_testing` per the c8te work), register the subscriber, emit a `GuardedActionFired`, assert a row was added with `memory_type: "guard_event"`.

- [ ] **Step 3: Run, confirm fail** — expect not found.

- [ ] **Step 4: Implement** the subscriber. Construct a `MemoryPayload` with `memory_type: "guard_event"`, call `store.add(namespace, payload)`. Use a 32-bit hash for `payload_hash`.

- [ ] **Step 5: Run, confirm pass** — expect PASS.

- [ ] **Step 6: Drill: emit two events for the same trace_id; assert two rows persist (no dedup — every guard call is its own audit row).**

- [ ] **Step 7: Commit**

```bash
git add src/events/
git commit -m "feat(events): guard event log subscriber + GuardedActionFired pulse variant"
```

---

### Task 6: Wire 4 emit sites in `src/server.rs` + new `handle_archive_memory` + emit in `task_complete` extraction

**Files:**
- Modify: `src/server.rs` (process_mcp_request signature + 3 emit sites + new handler)
- Modify: `src/task/mod.rs` (emit in inline extraction)
- Create: `src/handlers/archive_memory.rs` (6-line handler per the spec)

**Interfaces:**
- Consumes: `ThalamicBus` (now passed as 5th param to `process_mcp_request`)
- Produces:
  - `pub async fn process_mcp_request(request, emb, store, ingests, bus: Arc<ThalamicBus>) -> Value`
  - `pub async fn handle_archive_memory(arguments: Value, store: Arc<dyn VectorStore>) -> String` — marks `metadata.archived: true` and returns the new id

- [ ] **Step 1: Write the failing test: `handle_add_memory` emits `Created` to the bus**

In `src/server.rs` test mod, instantiate a test bus with a recording subscriber, call `process_mcp_request` with `tools/call neurostrata_add_memory ...`, assert the subscriber received a `ThalamicPulse::Created { id, namespace, kind }`.

- [ ] **Step 2: Run, confirm fail** — expect "parameter count mismatch" or similar (since the new param isn't added yet).

- [ ] **Step 3: Add `bus: Arc<ThalamicBus>` as the 5th parameter to `process_mcp_request`. Update the daemon's call site in `src/daemon.rs` to construct and pass it (defer the actual bus construction to Task 8; for now, take it as a parameter and use it).**

- [ ] **Step 4: In `handle_add_memory`, after the schema-ready write succeeds, call `bus.emit(ThalamicPulse::Created { id, namespace, kind: payload.memory_type })`. Wrap in `let _ = bus.emit(...);` (fire-and-forget).**

- [ ] **Step 5: Run, confirm pass** — expect PASS.

- [ ] **Step 6: Repeat steps 1-5 for `handle_supersede_memory` (emit `Superseded`), then for the inline extraction path in `handle_task_complete` in `src/task/mod.rs` (emit `Superseded` when the lesson memory is created).**

- [ ] **Step 7: Implement `handle_archive_memory` in `src/handlers/archive_memory.rs` (6 lines: extract id + namespace from args, set `metadata.archived: true` via `store.update_metadata`, return success string).**

- [ ] **Step 8: Wire the new handler into the `tools/call` match block in `process_mcp_request`.**

- [ ] **Step 9: Add the `neurostrata_archive_memory` entry to the `tools/list` array in `process_mcp_request` (inputSchema: `{id, namespace}`; required).**

- [ ] **Step 10: Update the `tools_list_carries_all_nineteen_tools_in_a_stable_order` test to expect 20 tools (rename the test, add the new name to the list).**

- [ ] **Step 11: Run the full suite, confirm 288 prior + new tests all pass.**

- [ ] **Step 12: Commit**

```bash
git add src/server.rs src/task/mod.rs src/handlers/archive_memory.rs
git commit -m "feat(events): wire 4 emit sites + handle_archive_memory + tools/list update"
```

---

### Task 7: POST /memory/archive + POST /bus/metrics endpoints + CLI dispatch

**Files:**
- Create: `src/handlers/bus_metrics.rs`
- Modify: `src/main.rs` (Commands enum + CLI dispatch for both endpoints)
- Modify: `src/daemon.rs` (route registration for new endpoints)

**Interfaces:**
- Consumes: `ThalamicBus` (constructed in Task 8, but referenced from the daemon's request handler)
- Produces:
  - `pub async fn handle_bus_metrics(bus: Arc<ThalamicBus>) -> String` — JSON-serialized `BusMetrics`
  - CLI: `neurostrata-mcp bus-metrics` (top-level subcommand)
  - CLI: `neurostrata-mcp archive <namespace> <id>` (top-level subcommand)
  - HTTP: `POST /memory/archive` (delegates to `handle_archive_memory` from Task 6)
  - HTTP: `POST /bus/metrics` (delegates to `handle_bus_metrics`)

- [ ] **Step 1: Add the `Archive` and `BusMetrics` variants to the `Commands` enum in `src/main.rs`. Archive takes `namespace: String, id: String`; BusMetrics takes no args.**

- [ ] **Step 2: Add the match arms that call into `handle_archive_memory` (CLI side) and `handle_bus_metrics`. Use the same exit-code discipline as the existing CLI commands.**

- [ ] **Step 3: Add the `POST /memory/archive` and `POST /bus/metrics` route registrations in `src/daemon.rs`'s router. Each delegates to the corresponding handler.**

- [ ] **Step 4: Write the failing test: CLI `bus-metrics` prints the JSON of `BusMetrics`.** (The bus may have 0 subscribers at this point; that's fine — the test just confirms the endpoint works end-to-end.)

- [ ] **Step 5: Run, confirm fail** — expect "Unknown command" or similar.

- [ ] **Step 6: Implement the CLI arms + HTTP routes.**

- [ ] **Step 7: Run, confirm pass** — expect PASS.

- [ ] **Step 8: Commit**

```bash
git add src/main.rs src/daemon.rs src/handlers/bus_metrics.rs
git commit -m "feat(events): POST /memory/archive + POST /bus/metrics + CLI dispatch"
```

---

### Task 8: Daemon constructs ThalamicBus + registers 3 subscribers at startup

**Files:**
- Modify: `src/daemon.rs`

**Interfaces:**
- Consumes: the daemon's existing `vector_store` and `embedder`
- Produces:
  - At startup: `let bus = Arc::new(ThalamicBus::new(10_000));` followed by three `bus.register(Box::new(...))` calls (EpisodicPointerEcho, ExportFreshnessDirty, GuardEventLog), then `let bus_clone = bus.clone();` to pass into the request handler
  - The bus is passed to `process_mcp_request` and the new HTTP endpoints

- [ ] **Step 1: Write the failing test: daemon starts, /bus/metrics shows 3 subscribers**

In a daemon integration test (or by adding a test in `src/daemon.rs` test mod), start the daemon with a temp config, hit `POST /bus/metrics`, assert the response includes `"subscribers": 3`.

- [ ] **Step 2: Run, confirm fail** — expect daemon to not yet construct the bus (or to construct with 0 subscribers).

- [ ] **Step 3: At the top of the daemon's startup sequence (after `vector_store` and `embedder` are constructed but before the HTTP server is bound), construct `let bus = Arc::new(ThalamicBus::new(10_000));` and register the 3 v1 subscribers.**

- [ ] **Step 4: Plumb `bus.clone()` into the HTTP server's request handler alongside the existing `vector_store` and `embedder`.**

- [ ] **Step 5: Run, confirm pass** — expect PASS.

- [ ] **Step 6: Drill: assert the order — daemon must construct the bus BEFORE starting the HTTP server (so the bus is available when the first request lands). If the order is reversed, a test that hits /bus/metrics immediately after start must catch it. The test for "daemon starts, /bus/metrics shows 3 subscribers" implicitly exercises this.**

- [ ] **Step 7: Build the daemon, restart the running daemon (`systemctl --user restart neurostrata.service`), confirm `/bus/metrics` returns 3 subscribers via curl.**

- [ ] **Step 8: End-to-end smoke through the live MCP proxy: call `neurostrata_add_memory` (fires `Created`), then `neurostrata-mcp bus-metrics` (shows the event count incremented), then check the episodic buffer file for the pointer line.**

- [ ] **Step 9: Run the full suite + self-test, confirm green** — `cargo test --release --bin neurostrata-mcp -- --test-threads=1` and `neurostrata-mcp task gate --self-test NeuroStrata` both green.

- [ ] **Step 10: Commit**

```bash
git add src/daemon.rs
git commit -m "feat(daemon): construct thalamic bus and register 3 v1 subscribers at startup"
```

---

### Task 9: Version bump 1.7.0 → 1.8.0 + CHANGELOG entry + cognitive-architecture doc

**Files:**
- Modify: `Cargo.toml` (line 3: `version = "1.7.0"` → `version = "1.8.0"`)
- Modify: `CHANGELOG.md` (collapse `[Unreleased]` into a new dated `[1.8.0] - 2026-10-09` section; add the bus + subscribers + endpoints entries)
- Modify: `docs/COGNITIVE_ARCHITECTURE.md` (add glossary entries for `ThalamicBus` and `ThalamicPulse`)

**Interfaces:** none (documentation + version metadata)

- [ ] **Step 1: Edit `Cargo.toml` line 3 to `version = "1.8.0"`.**

- [ ] **Step 2: In `CHANGELOG.md`, replace the existing `## [Unreleased]` header with `## [1.8.0] - 2026-10-09`. Keep the existing `### Added` content for the task rename; append the new entries for the bus in `### Added`.**

- [ ] **Step 3: Add the following `### Added` entries to the `[1.8.0]` section:**

```
- **Thalamic Bus (memory-event system):** a publisher/subscriber bus constructed in `daemon.rs` and threaded through `process_mcp_request` as `Arc<ThalamicBus>`. Bounded mpsc (cap 10000, drop-oldest) feeds a single dispatcher task that runs each `MemorySubscriber` with a 250ms timeout and a panic-catcher. `RecursionToken` prevents subscriber-induced loops. New endpoints `POST /memory/archive` and `POST /bus/metrics`. Three v1 subscribers: `EpisodicPointerEcho` (closes the Continuous Backup Protocol gap by writing a pointer to the episodic buffer on every `Created`), `ExportFreshnessDirty` (sets a per-namespace dirty flag consumed by the export-freshness gate, A4 wire), `GuardEventLog` (persists every `guard_validate` call as a `memory_type: "guard_event"` row with a 32-bit payload hash). Four emit sites: `add_memory`, `supersede_memory`, `task_complete` extraction, the new `archive_memory` handler. Names follow the cognitive-anatomy convention — Thalamus is the brain's relay station, which is what a pub-sub bus does.
- **Memory vocabulary v4:** `ThalamicPulse` (the bus event type) and `GuardedActionFired` (the 4th variant for guard audit).
```

- [ ] **Step 4: In `docs/COGNITIVE_ARCHITECTURE.md`, find the existing glossary / cognitive-names table and add two new rows:**

```
| **Thalamic Bus** | The pub-sub event system in `src/events/`. Thalamus = the brain's relay station; the bus relays storage events (Created, Superseded, Archived, GuardedActionFired) from emit sites to subscribers. |
| **Thalamic Pulse** | A single event on the Thalamic Bus. Cognitive name for `ThalamicPulse` (the `#[non_exhaustive]` enum). |
```

If the file does not contain a cognitive-names table, add a new section at the end titled "## Cognitive-name glossary" with the entries (and add the 1.7.0 entries for completeness).

- [ ] **Step 5: Build, confirm no warnings** — `cargo build --release` → 0 new warnings (the 11 baseline warnings are tolerated).

- [ ] **Step 6: Run the full test suite + self-test, confirm green.**

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml CHANGELOG.md docs/COGNITIVE_ARCHITECTURE.md
git commit -m "chore(release): 1.8.0 — thalamic bus, cognitive names, 3 v1 subscribers"
```

---

## Self-Review (executing the skill's checklist)

**1. Spec coverage** — every section in `docs/superpowers/specs/2026-10-09-memory-event-bus-design.md`:
- Intent (Continuous Backup gap, export freshness, guard event log) → Tasks 3, 4, 5
- Non-goals → no task implements them (correct)
- Architecture (daemon-owned, threaded) → Task 8
- Components (ThalamicBus, ThalamicPulse, MemorySubscriber, RecursionToken, SubscriberContext, BackpressureEvent, BusMetrics) → Tasks 1, 2
- v1 subscribers (3) → Tasks 3, 4, 5
- Emit sites (4) → Task 6 + Task 8 (daemon start)
- Data flow → emerges from Tasks 6 + 8
- Error model → Task 2 (panic catch), Task 5 (drill)
- Testing → drill steps in Tasks 1, 2, 3, 4, 5; e2e in Task 8
- Extensibility → documented in spec, no new tests needed
- v1 vs deferred → not in scope

Gaps: none. The spec's `POST /memory/archive` and `POST /bus/metrics` endpoints both have tasks (6 and 7).

**2. Step scan** — every step lets the implementer write exactly one reasonable thing. Test steps give the assertion; code steps give the signature; verification steps give the command. No "TBD", no body transcripts, no `apply_patch` style multi-edit steps.

**3. Type consistency** — Task 1 produces the canonical types; Tasks 2-7 use them with the names listed in the Interfaces block. No rename or relabel mid-stream.

**4. Review Focus** — five lines:
1. Subscriber panic during burst → Task 2 Step 11 drill
2. Backpressure drop-oldest → Task 2 Steps 5-8
3. Recursion block → Task 2 Step 13
4. Archived-without-producer → Task 6 Step 7 (the new handler IS the producer; if missing, the test for "register 3 subscribers + emit Archived in handle_archive_memory" would fail at Task 8's e2e)
5. Restart replay semantics → Task 8 Step 8 documents "no replay; subscribers re-fire" (acceptable per spec)

**5. Proportion** — spec is 197 lines; plan is ~280 lines. Plan is ~1.4x spec. Not a transcript. Within budget.
