# Incremental AST Freshness Watcher Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a daemon-native filesystem watcher that feeds the existing generation ingest so registered source roots stay fresh without client polling.

**Architecture:** A client-agnostic Rust core (`src/ingest_watch.rs`) owns registrations, debounce/coalescing, reconciliation, backoff, and status behind three ports (`WatchEventSource`, `WatchRegistrationStore`, `IngestTrigger`). Concrete adapters (notify-backed watcher, strict-JSON file store, `IngestJobs`-backed trigger) are wired only at the daemon composition root. MCP tools and daemon HTTP routes are thin JSON shims to the same core methods.

**Tech Stack:** Rust 2021, tokio 1.53 (`full`), `notify` 8.x (new dep), axum 0.8, serde/serde_json, `ignore` 0.4.34 walker, LadybugDB via existing `IngestJobs`.

**Spec:** `docs/superpowers/specs/2026-11-20-incremental-freshness-watcher-design.md`

## Global Constraints

- Quiet period **300 ms**; max debounce window **2 s**; backoff steps **1 s → 2 s → 4 s → 8 s → 16 s → 30 s** (cap), reset on success.
- Registration persistence: **strict JSON only**, `<config-dir>/watch_registrations.json`, `schema_version: 1`, atomic owner-only (`0o600`) writes.
- Fail-closed: malformed/invalid/J6-failing persisted entries rejected with a traced reason, never silently activated. Corrupt/unreadable registry ⇒ empty active set + trace; missing file ⇒ empty registry.
- **J6:** every registration path (manual, auto-register on ingest success, unwatch, load activation) passes `check_caller_path(raw, startup_root, allowed_external_dirs)` (`src/daemon.rs:607-611`, `pub(crate)`).
- Pluggable interface architecture: Universal Core + Client Shims; ports/adapters; no client-specific code in core; no LadybugStore downcast in this feature; plugins may only expose core status/control.
- **No parallel partial ingest path and no polling.** All changes flow through `IngestJobs::run` (which already does SHA-256 skip and generation carry-forward).
- No `formal_kernel.rs` change — no gate/ACL/consensus invariant; pure decisions extracted as testable functions.
- Native NeuroStrata terminology only; never name files/features/symbols after the comparison project.
- Verification lane: **focused tests + `cargo check` only**. No full-workspace `cargo test`. No sleep-polling in tests. Prettier must pass (`npx prettier --write` on touched Markdown).
- No PR until implementation, docs, focused tests, local verification, and independent review are clean. No deferral/TBD/phase-2 language in code or docs.

## Review Focus

- **Event storm under continuous writes** → coalesce to one path set and flush at the 2 s max window. Test: max-window flush with injectable event source (Task 3).
- **Ingest failure on a dirty root** → root stays dirty, backoff 1 s→30 s, retry converges, daemon never crashes. Test: backoff transition + dirty-during-run (Task 3).
- **Malformed or J6-unauthorized persisted registration** → rejected fail-closed with trace, never activated; valid siblings still load. Test: fail-closed load matrix (Task 2).
- **DB/WAL/lock artifacts under a watched root** → filtered before coalescing, no self-trigger loop. Test: eligibility predicate parity with ingest walker (Task 1).
- **Shutdown with in-flight watch-triggered ingest** → no lost work; watch loop stops accepting flushes, `IngestJobs::wait_idle` covers the walk. Test: shutdown-with-pending flush (Task 3); verified at composition root (Task 4).

---

### Task 1: Core vocabulary, ports, pure decisions, shared eligibility predicate

**Files:**

- Modify: `Cargo.toml` (add `notify = "8.2"`; verified stable 8.2.1, rust-version 1.85)
- Create: `src/ingest_watch.rs` (types + traits + pure functions only in this task)
- Modify: `src/parser/ingest.rs` (extract `is_ingest_eligible_path`; replace both inline skip arrays/filter loops)
- Modify: `src/main.rs` (add `mod ingest_watch;` next to `mod ingest_jobs;`)
- Test: inline `#[cfg(test)] mod tests` in `src/ingest_watch.rs` and `src/parser/ingest.rs`

**Interfaces:**

- Consumes: `ParserSchema`, `IngestProgress`/`IngestJobs` (Task 4 wires them), `check_caller_path` (Task 2).
- Produces (exact, used verbatim by later tasks):

  - `pub const QUIET_PERIOD: Duration = Duration::from_millis(300);`
  - `pub const MAX_WINDOW: Duration = Duration::from_secs(2);`
  - `pub const BACKOFF_STEPS_SECS: &[u64] = &[1, 2, 4, 8, 16, 30];`
  - `pub struct Registration { pub namespace: String, pub canonical_path: PathBuf, pub created_at_unix: u64, pub updated_at_unix: u64 }` + `Registration::key(&self) -> (String, PathBuf)`
  - `pub enum WatchEventKind { Create, Modify, Remove, Rename }`
  - `pub struct WatchEvent { pub root: PathBuf, pub path: PathBuf, pub kind: WatchEventKind, pub new_path: Option<PathBuf> }` (`new_path` is `Some` only for `Rename`: old path in `path`, destination in `new_path`; `None` for all other kinds)
  - `pub struct PathEffect { pub path: PathBuf, pub upsert: bool }`
  - `#[async_trait] pub trait WatchEventSource: Send { async fn recv(&mut self) -> Option<WatchEvent>; fn watch_root(&mut self, root: &Path) -> Result<(), String>; fn unwatch_root(&mut self, root: &Path) -> Result<(), String>; }`
  - `pub trait WatchRegistrationStore: Send + Sync { fn load(&self) -> anyhow::Result<Vec<Registration>>; fn save(&self, regs: &[Registration]) -> anyhow::Result<()>; }`
  - `pub struct TriggerOutcome { pub state: TriggerState, pub error: Option<String> }`; `pub enum TriggerState { Finished, Failed }`
  - `#[async_trait] pub trait IngestTrigger: Send + Sync { async fn trigger(&self, namespace: &str, canonical_path: &str) -> anyhow::Result<TriggerOutcome>; }`
  - Pure: `normalize_event(WatchEvent) -> Vec<PathEffect>` (rename ⇒ remove old + upsert new), `should_flush(quiet_elapsed: Duration, window_elapsed: Duration) -> bool`, `next_backoff(attempt: usize) -> Duration`
  - `pub fn is_ingest_eligible_path(path: &Path, root: &Path) -> bool` and `pub const SKIPPED_DIRS: &[&str]` exported from `src/parser/ingest.rs`

- [ ] **Step 1: Write failing tests for pure decisions and eligibility**

In `src/ingest_watch.rs` tests assert: `normalize_event` on `Rename` yields exactly two effects (`upsert=false` for `event.path`, `upsert=true` for `event.new_path`, which must differ); on `Create`/`Modify`/`Remove` yields one effect with the matching `upsert` flag. `should_flush` false below 300 ms quiet and below 2 s window; true at 300 ms quiet; true at 2 s window even under continuous events. `next_backoff(0..5)` equals 1,2,4,8,16,30 seconds and `next_backoff(6)` stays 30 s.

In `src/parser/ingest.rs` tests assert: all 19 existing skip names (verbatim: `node_modules, target, vendor, .venv, venv, env, .env, dist, build, out, .dolt, .git, .next, .nuxt, __pycache__, .fastembed_cache, .idea, .vscode, coverage`) rejected anywhere in a path under root; `*.db`, `*.wal`, and lock/checkpoint sidecars rejected; a plain `src/lib.rs` under root accepted; paths outside root rejected.

- [ ] **Step 2: Run focused tests to verify failure**

Run: `cargo test --bin neurostrata-mcp ingest_watch::tests -- --nocapture` and `cargo test --bin neurostrata-mcp parser::ingest::tests::eligibility -- --nocapture`
Expected: compile failure — missing types/functions.

- [ ] **Step 3: Implement types, traits, pure functions, and shared predicate**

Add the exact interfaces above to `src/ingest_watch.rs`. In `src/parser/ingest.rs`, move the duplicated 19-name arrays from `run_producer` (~`:1267-1287`/`:1316-1331`) and `dry_run_directory` (~`:5851-5871`/`:5901-5916`) into one `SKIPPED_DIRS` const and one `is_ingest_eligible_path` predicate; both call sites delegate to it. Preserve existing skip semantics; add DB/WAL/lock rejection. Add dependency and module declaration.

- [ ] **Step 4: Run focused tests and `cargo check`**

Run: the two commands from Step 2, then `cargo check`
Expected: all assertions pass; `cargo check` clean.

- [ ] **Step 5: Prettier + commit**

Run: `npx prettier --write src/ingest_watch.rs src/parser/ingest.rs src/main.rs Cargo.toml`
Then `git add` those files and commit `feat(watch): core ports and shared eligibility predicate`.

---

### Task 2: Strict-JSON registration store, registration CRUD, J6, status snapshot

**Files:**

- Modify: `src/ingest_watch.rs` (add `JsonFileRegistrationStore`, `CoreState`, `IngestWatchService` registration/status methods)
- Test: inline tests in `src/ingest_watch.rs`

**Interfaces:**

- Consumes: all Task 1 types; `check_caller_path` (`src/daemon.rs:607-611`); `Config::default_path()` (`src/config.rs:152-158`) for config-dir resolution; atomic-write pattern from `Config::save_to` (`src/config.rs:223-280`).
- Produces:

  - `pub struct JsonFileRegistrationStore { pub path: PathBuf }` with `pub fn new(path: PathBuf) -> Self` implementing `WatchRegistrationStore`.
  - `pub struct RootRuntime { pub registration: Registration, pub dirty: bool, pub pending: HashSet<PathBuf>, pub last_event_unix: Option<u64>, pub last_trigger_unix: Option<u64>, pub last_outcome: Option<TriggerOutcome>, pub backoff_attempt: usize, pub next_retry_unix: Option<u64>, pub watch_active: bool }` (fields `pub(crate)` within module tests as needed)
  - `pub struct RootStatus { pub namespace: String, pub canonical_path: PathBuf, pub dirty: bool, pub pending_paths_count: usize, pub last_event_unix: Option<u64>, pub last_trigger_unix: Option<u64>, pub last_ingest_state: Option<TriggerState>, pub last_ingest_finished_unix: Option<u64>, pub backoff_attempt: usize, pub next_retry_unix: Option<u64>, pub watch_active: bool }`
  - `pub struct WatchStatusReport { pub roots: Vec<RootStatus> }`
  - `pub struct IngestWatchService` with:
    - `pub fn new(events: Box<dyn WatchEventSource>, store: Arc<dyn WatchRegistrationStore>, trigger: Arc<dyn IngestTrigger>, startup_root: PathBuf, allowed_external_dirs: Vec<PathBuf>) -> Self`
    - `pub fn register(&self, namespace: &str, raw: &Path) -> anyhow::Result<Registration>` (J6, canonicalize, idempotent by key, persist, `events.watch_root`)
    - `pub fn unregister(&self, namespace: &str, raw: &Path) -> anyhow::Result<()>` (J6, remove from store/state, `events.unwatch_root` when unreferenced)
    - `pub fn on_ingest_success(&self, namespace: &str, canonical: &Path) -> anyhow::Result<()>` (idempotent auto-register hook)
    - `pub fn status(&self, namespace: Option<&str>) -> WatchStatusReport`
    - `pub async fn start(&self) -> anyhow::Result<()>` (load via store, J6 re-check each path, activate watch, initialize runtime; does **not** start the event loop)
  - Core holds ports behind `Arc`; `events` behind `tokio::sync::Mutex<Box<dyn WatchEventSource>>` so the later event loop can own `recv`.

- [ ] **Step 1: Write failing tests for persistence and CRUD**

Tests: (a) save→load round-trip preserves namespace, canonical_path, timestamps and `schema_version: 1`; (b) a document with wrong `schema_version` or an entry missing `updated_at_unix` is rejected fail-closed with a traced reason and produces no active registration while valid siblings still load; (c) duplicate `(namespace, canonical_path)` entries de-duplicate deterministically; (d) a path outside `startup_root`/`allowed_external_dirs` is refused at `register`, `unregister`, and load-activation; (e) corrupt/unreadable JSON yields empty active set and no panic; (f) `on_ingest_success` twice yields one registration; (g) `status` reports one `RootStatus` per active root with `dirty=false`, `watch_active=true`, `pending_paths_count=0`.

Use `tempfile::tempdir()` and construct the service with a `TestEventSource` stub (a channel-backed `WatchEventSource` implemented in the test module) and a no-op `IngestTrigger` stub. Do not start the event loop in these tests.

- [ ] **Step 2: Run focused tests to verify failure**

Run: `cargo test --bin neurostrata-mcp ingest_watch::tests -- --nocapture`
Expected: compile failure — missing service/store types.

- [ ] **Step 3: Implement store and core CRUD**

Implement `JsonFileRegistrationStore` with strict serde structs (`deny_unknown_fields`, explicit `schema_version`), atomic 0o600 temp-write + rename mirroring `Config::save_to`, fail-closed `load` that traces (`tracing::warn!`) each rejected entry and returns only valid ones. Implement `IngestWatchService` fields/methods above. Core calls `check_caller_path` on every path before mutation/activation. No parser/embedder/vector-store/LadybugStore types in this module.

- [ ] **Step 4: Run focused tests and `cargo check`**

Run: `cargo test --bin neurostrata-mcp ingest_watch::tests -- --nocapture` then `cargo check`
Expected: all tests pass; no warnings from new code.

- [ ] **Step 5: Prettier + commit**

Run: `npx prettier --write src/ingest_watch.rs`, stage, commit `feat(watch): strict-JSON registration store and J6 registration CRUD`.

---

### Task 3: Event adapters, debounce loop, reconciliation, backoff, status runtime

**Files:**

- Modify: `src/ingest_watch.rs` (add `NotifyEventSource`, `TestEventSource`, `IngestJobsTrigger`, `IngestWatchService::run`, runtime state transitions)
- Test: inline unit tests + one `#[tokio::test]` real-watcher integration test in `src/ingest_watch.rs`

**Interfaces:**

- Consumes: Task 1 + Task 2 interfaces verbatim; `IngestJobs::run` (`src/ingest_jobs.rs:168-175`, returns `IngestProgress`); `ingest_directory` schema loading via `include_str!("schema.json")` + `ParserSchema::load` (pattern at `src/daemon.rs:1403-1404`); `notify::RecommendedWatcher`.
- Produces:

  - `pub struct NotifyEventSource { watcher: notify::RecommendedWatcher, rx: tokio::sync::mpsc::UnboundedReceiver<WatchEvent> }` implementing `WatchEventSource`; constructor `pub fn new() -> anyhow::Result<Self>`.
  - `pub struct TestEventSource { pub tx: tokio::sync::mpsc::UnboundedSender<WatchEvent>, rx: tokio::sync::mpsc::UnboundedReceiver<WatchEvent> }` implementing `WatchEventSource`; `pub fn channel() -> Self`.
  - `pub struct IngestJobsTrigger { pub ingests: Arc<crate::ingest_jobs::IngestJobs>, pub schema: crate::parser::ParserSchema, pub embedder: Arc<dyn crate::traits::Embedder>, pub vector_store: Arc<dyn crate::traits::VectorStore> }` implementing `IngestTrigger`; constructor `pub fn new(...)`.
  - `pub async fn run(self: Arc<Self>, mut shutdown: tokio::sync::oneshot::Receiver<()>)` — event loop using `tokio::select!` over `events.recv()`, per-root flush deadlines, backoff deadlines, and shutdown.
  - `pub async fn reconcile(&self)` — one full skip-fast trigger per active root.

- [ ] **Step 1: Write failing tests with the injectable source (no real FS events)**

Tests via `TestEventSource::channel()`: (a) two events inside 300 ms coalesce into one flush with a 2-path set; (b) continuous events force flush at 2 s max window; (c) ineligible paths (`.git`, build dir, `*.db`, `*.wal`, lock sidecar) never enter pending; (d) rename produces remove+upsert effects; (e) failed trigger leaves `dirty=true`, `backoff_attempt` advances 1→2→...→5 and `next_backoff(6)` is capped, success resets to 0; (f) event during active run sets dirty and yields exactly one subsequent trigger after completion with no overlap; (g) `reconcile()` emits one trigger per active root; (h) shutdown with pending events stops accepting flushes and leaves durable state unchanged.

Drive the loop with a manual `advance`/`tick` seam if needed (pure `should_flush` is already covered in Task 1); avoid `tokio::time::sleep` in tests. Use a `RecordingTrigger` stub that records calls and returns scripted `TriggerOutcome`s.

- [ ] **Step 2: Run focused tests to verify failure**

Run: `cargo test --bin neurostrata-mcp ingest_watch::tests -- --nocapture`
Expected: compile failure — missing adapters/loop.

- [ ] **Step 3: Implement adapters and event loop**

Implement `NotifyEventSource`: translate `notify` create/modify/remove/rename kinds into `WatchEvent`; for rename populate `path` with the old path and `new_path` with the destination from notify's rename payload; watcher callback sends into the unbounded channel; `watch_root`/`unwatch_root` delegate to `notify::Watcher`. Implement `IngestJobsTrigger` mapping `IngestProgress.state` to `TriggerState` and error text to `TriggerOutcome.error`. Implement `run`: filter through `is_ingest_eligible_path`, normalize to `PathEffect`s, insert into per-root `pending`, compute flush deadline from first event (max window) and last event (quiet period), call `should_flush`/`next_backoff`, trigger via port, update runtime/status, retry on failure without crashing. On shutdown, stop accepting events; rely on `IngestJobs::wait_idle` at the daemon layer for in-flight walks.

- [ ] **Step 4: Add and run the single real-watcher integration test**

One `#[tokio::test]` in a `tempfile::tempdir()` root: register the root with `NotifyEventSource`, create/modify/delete/rename an eligible file, assert an ingest trigger is observed; create a `*.db`/`.git` file and assert no trigger. The test must run and pass; no `#[ignore]`. If the environment cannot support a real watcher, report BLOCKED rather than skipping.

Run: `cargo test --bin neurostrata-mcp ingest_watch::tests::real_watcher -- --nocapture`, then `cargo check`.

- [ ] **Step 5: Prettier + commit**

Run: `npx prettier --write src/ingest_watch.rs`, stage, commit `feat(watch): event adapters, debounce loop, reconciliation and backoff`.

---

### Task 4: Daemon composition root and HTTP parity shims

**Files:**

- Modify: `src/daemon.rs` (`AppState` field, routes, handlers, startup wiring, shutdown, `/ingest` auto-register, MCP-with-watch call)
- Test: inline focused tests in `src/daemon.rs`

**Interfaces:**

- Consumes: Task 1–3 interfaces verbatim; `AppState` (`src/daemon.rs:398-435`); `start_daemon_on` (`src/daemon.rs:991-1295`); `check_caller_path` (`src/daemon.rs:607-611`); `handle_ingest` success path (`src/daemon.rs:1389-1432`); router chain (`src/daemon.rs:1093-1131`); daemon MCP handler call (`src/daemon.rs:1809`).
- Produces:

  - `AppState.watch: Arc<crate::ingest_watch::IngestWatchService>`
  - `POST /ingest/watch/unwatch` → `Json<WatchUnwatchReq { namespace: String, dir_path: String }>` → `state.watch.unregister(...)`; returns `{ "removed": bool, "namespace": ..., "canonical_path": ... }` or the standard `(StatusCode, String)` policy error.
  - `GET /ingest/watch/status` → `state.watch.status(query namespace filter)` → JSON `WatchStatusReport`.
  - `start_daemon_on` builds `NotifyEventSource` + `JsonFileRegistrationStore` (path = `Config::default_path()?.parent()` joined with `watch_registrations.json`) + `IngestJobsTrigger` (shipped schema, embedder, vector store), constructs the service, calls `start()`, spawns `run(shutdown_rx)`, and awaits it in graceful shutdown alongside `ingests.wait_idle(...)`.
  - `handle_ingest` calls `state.watch.on_ingest_success(&namespace, &dir)` after a successful `state.ingests.run(...)`.

- [ ] **Step 1: Write focused tests for shim request mapping**

Assert: (a) unwatch shim maps a valid `dir_path` to core `unregister` and returns `removed=true`; (b) a J6-refusing path yields the same status/error class as `handle_ingest` refusals; (c) status shim serializes `WatchStatusReport` with all `RootStatus` fields and honors the optional namespace filter; (d) `/ingest` success invokes `on_ingest_success` with the resolved canonical path and namespace. Use existing `tower::ServiceExt::oneshot` test style already present in daemon tests.

- [ ] **Step 2: Run focused tests to verify failure**

Run: `cargo test --bin neurostrata-mcp daemon::tests::watch -- --nocapture`
Expected: compile failure — missing field/routes/handlers.

- [ ] **Step 3: Wire composition root and shims**

Add field, construct/start service in `start_daemon_on`, register routes under the same auth middleware (`.layer(axum::middleware::from_fn_with_state(state, auth_middleware))`), add thin handlers with no independent logic, invoke auto-register hook, and wire shutdown.

- [ ] **Step 4: Run focused tests and `cargo check`**

Run: `cargo test --bin neurostrata-mcp daemon::tests::watch -- --nocapture` then `cargo check`
Expected: all pass; no new warnings.

- [ ] **Step 5: Prettier + commit**

Run: `npx prettier --write src/daemon.rs`, stage, commit `feat(watch): daemon watch composition and HTTP parity shims`.

---

### Task 5: MCP thin shims and auto-registration hook

**Files:**

- Modify: `src/server.rs` (tool declarations, dispatch, handlers, `process_mcp_request_with_watch`, `handle_ingest_directory` watch parameter, stable-order test)
- Modify: `src/daemon.rs` (MCP handler call at `:1809` only)
- Test: inline focused tests in `src/server.rs`

**Interfaces:**

- Consumes: Task 2–4 interfaces; `process_mcp_request` (`src/server.rs:105-112`); `handle_ingest_directory` (`src/server.rs:6343-6448`, success branch `:6404-6445`); tools/list declaration block (`src/server.rs:147-1442`); dispatch match (`src/server.rs:1537`); stable-order test (`src/server.rs:18633`).
- Produces:

  - `pub async fn process_mcp_request_with_watch(request, emb, store, ingests, deduplication_checker, bus, watch: Option<Arc<crate::ingest_watch::IngestWatchService>>) -> Value`
  - Existing `process_mcp_request` kept as a thin wrapper calling `_with_watch(..., None)` so the ~23 existing test call sites stay unchanged.
  - `neurostrata_unwatch` — input `{ namespace: String, dir_path: String }` (required, `dir_path` matches ingest tool), output `{ removed, namespace, canonical_path }`.
  - `neurostrata_watch_status` — optional `{ namespace }`, output `WatchStatusReport` as JSON.
  - `handle_ingest_directory(..., bus: Option<Arc<ThalamicBus>>, watch: Option<Arc<IngestWatchService>>)`; in the `Ok(progress)` branch call `watch.on_ingest_success(&namespace, &canonical_dir)` before formatting success text.
  - daemon MCP handler at `src/daemon.rs:1809` passes `Some(state.watch.clone())` to `process_mcp_request_with_watch`.

- [ ] **Step 1: Write failing tests for the two shims and auto-register**

With a `TestEventSource`/stub trigger service: (a) `tools/list` contains exactly the two new names in stable positions; update the existing ordered-name assertion accordingly; (b) `tools/call neurostrata_unwatch` returns `removed=true` for a registered pair and the policy error for a J6-refusing path; (c) `tools/call neurostrata_watch_status` returns the report and is read-only; (d) a successful `tools/call neurostrata_ingest_directory` registers the root (visible via `watch_status`) and a failed one does not.

- [ ] **Step 2: Run focused tests to verify failure**

Run: `cargo test --bin neurostrata-mcp server::tests::watch -- --nocapture`
Expected: compile failure — tools/handlers absent.

- [ ] **Step 3: Implement shims**

Add the `_with_watch` entry point with the original as a wrapper; add the two tool declarations and dispatch arms; add two handler functions delegating to core (no watcher logic in `server.rs`); thread the optional watch service into `handle_ingest_directory` and call the hook on success; update the ordered-name test; switch the daemon MCP handler to pass `Some(state.watch.clone())`.

- [ ] **Step 4: Run focused tests and `cargo check`**

Run: `cargo test --bin neurostrata-mcp server::tests::watch -- --nocapture` and `cargo test --bin neurostrata-mcp server::tests::tools_list -- --nocapture`, then `cargo check`
Expected: all pass, including the updated stable-order assertion; no new warnings.

- [ ] **Step 5: Prettier + commit**

Run: `npx prettier --write src/server.rs`, stage, commit `feat(watch): MCP status/unwatch shims and ingest auto-registration`.

---

### Final verification (not a task dispatched to an implementer)

- `cargo check`
- Focused test filters from Tasks 1–5
- `npx prettier --check` on all touched files
- Whole-branch review via `review-package docs/superpowers/plans/2026-11-20-incremental-freshness-watcher.md <merge-base> HEAD`
- Then `finishing-a-development-branch`; no PR until all gates and independent review are clean.
