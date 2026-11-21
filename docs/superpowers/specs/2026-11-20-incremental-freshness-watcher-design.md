# Daemon-Native Incremental AST Freshness (Filesystem Watcher → Generation Ingest)

**Date:** 2026-11-20  
**Status:** Approved design, implementation-ready specification (design artifact only)  
**Task:** `neurostrata-1q6s` (namespace `NeuroStrata`)  
**Owner directive:** Never defer. No PR is published until implementation, docs, focused tests, local verification, and an independent review are all clean.

## 1. Context and motivation

NeuroStrata’s structural graph is produced by generation-scoped ingestion (`neurostrata_ingest_directory` / the daemon ingest path). Today freshness is pull-based: a client must explicitly request ingest. The graph therefore drifts stale between explicit runs, which degrades graph-first navigation and any downstream feature that relies on current AST/call/import edges.

This spec adds daemon-native, push-driven freshness: the long-lived LadybugDB daemon observes registered source roots, coalesces filesystem events, and triggers the existing generation ingest so active structural state converges without a separate, partial update mechanism.

### Verified source evidence (grounding)

| Area                                    | Evidence (verified)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           | Implication for this design                                                                                                                                                                                |
| --------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Generation-scoped IDs & versioning      | `src/parser/generation.rs`: `generation_scoped_id(namespace,generation,logical_path)` (`Namespace::generation::logical_path`), `generation_key(namespace,source_key)`, `source_key_for` usage via ingest, `sha256_hex`, `PARSER_LOGIC_VERSION = 3`, `ingest_version(schema_fingerprint)`, `version_gate_tripped`, stable `symbol_id` hashed from full definition text (`span_sha256`). Previous generations remain history; only the active generation is reachable.                                                                          | Watch-triggered runs must preserve generation semantics and must not introduce a second identity/publication scheme.                                                                                       |
| Incremental, skip-fast ingest           | `src/parser/ingest.rs`: unchanged-file SHA-256 skip (`prev_hash == file_sha256` → `ProcessedEntry::UnchangedFile`, ~725–731), carry-forward of unchanged nodes from the previous generation (~2157+), whole-tree unchanged short-circuit that keeps the active generation when the walk finds the same nodes/kinds and re-reads no content (~2127–2147), and version gate / forced reparse (~1966–1985). Producer walk skips `.git`, build artifacts, DB/cache dirs, etc. (`skipped_dirs`, ~1267–1288) and scans for secrets before indexing. | A full re-walk on event batches is cheap in the common unchanged case and is the correct way to keep the active generation consistent; event batches are triggers, not partial-parse inputs.               |
| Detached ingest jobs                    | `src/ingest_jobs.rs`: `IngestJobs` runs one detached `tokio::spawn` walk per namespace, keyed by canonical dir; `attach_to_running` refuses a second directory for a running namespace and attaches same-dir callers to the running job; `IngestProgress` watch channel reports state/counts; `wait_idle(limit)` waits for running walks (used by daemon graceful shutdown). `run(namespace, dir, schema, embedder, vector_store)` is the reuse seam.                                                                                         | Watcher must reuse `IngestJobs::run`/progress/wait semantics; no parallel ingest execution path.                                                                                                           |
| Daemon authority & boundaries           | `src/daemon.rs`: single HTTP daemon (`start_daemon_on`, default `127.0.0.1:34343`) holds LadybugDB; `AppState` owns `ingests: Arc<IngestJobs>`, bus, embedder, vector store, shutdown, `startup_root`, `allowed_external_dirs`; caller-path policy enforced by `check_caller_path` / `caller_path_refusal` (J6) on path-taking routes (`/ingest`, `/backup`, `/export-graph`, `/tasks/import`, etc.); `/ingest` loads shipped `schema.json` and calls `ingests.run(...)`. Shutdown waits for in-flight ingests via `ingests.wait_idle(...)`.  | Watch registrations, watch-triggered ingests, and new watch surfaces are daemon-owned and must pass J6. Watch-triggered walks already count as `IngestJobs` work for shutdown.                             |
| Configuration & persistence conventions | `src/config.rs`: `Config` (`db_path`, `project_contexts`, `allowed_external_dirs`, `extra` via `#[serde(flatten)]`, atomic `save_to` with restrictive permissions, `load_from`/`default_path()` → `~/.config/neurostrata/config.json`). `allowed_external_dirs` is the only escape hatch outside the startup/project root.                                                                                                                                                                                                                    | Watch registrations persist as strict JSON under the daemon config directory with atomic, owner-only writes consistent with `Config::save_to` semantics.                                                   |
| Dependency baseline                     | `Cargo.toml`: no filesystem-watch crate is present.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           | Add the `notify` filesystem-watch crate, whose `notify::RecommendedWatcher` is used by the concrete `WatchEventSource` adapter.                                                                            |
| MCP/server tool surface                 | `src/server.rs`: `neurostrata_ingest_directory` dispatches to `handle_ingest_directory(..., ingests)` and shares `IngestJobs` with the daemon path.                                                                                                                                                                                                                                                                                                                                                                                           | Auto-registration on successful ingest and new `neurostrata_unwatch` / `neurostrata_watch_status` tools live in the MCP/server surface (and corresponding daemon HTTP endpoints where parity is required). |

## 2. Goals and non-goals

### Goals

1. **Daemon-native freshness:** The daemon maintains up-to-date structural state for explicitly registered roots by reacting to filesystem changes without client polling.
2. **Reuse the existing ingest engine:** Feed event batches into the existing generation ingest (`IngestJobs::run` → `ingest_directory`) so SHA-256 skip, carry-forward, unchanged short-circuit, version gating, secret scanning, and generation publication remain the single source of truth.
3. **Safe, bounded background behavior:** Debounced/coalesced triggers, per-root isolation, no daemon crashes on watcher/ingest failures, and bounded retry/backoff.
4. **Durable intent:** Watch registrations persist across daemon restarts and are reconciled on startup to cover missed events.
5. **Operator visibility and control:** Status inspection and explicit unwatch/removal, with J6-enforced path authority on every registration/removal path.
6. **Testability:** Pure, deterministic decision functions for coalescing/flush/backoff/eligibility and injectable event sources for unit tests; focused verification only.

### Non-goals

- **No parallel or partial AST-update path.** Events do not mutate AST rows directly and do not bypass generation ingest.
- **No polling-based freshness loop.** Freshness is event-driven with startup reconciliation; there is no periodic rescan scheduler.
- **No client/UI redesign.** Tauri/clients remain HTTP consumers of the daemon; this change is daemon/server-side.
- **No cross-namespace merge semantics changes.** Per-root namespace isolation and existing `IngestJobs` namespace rules are preserved.
- **No `formal_kernel.rs` change.** This feature introduces no gate, ACL, or consensus invariant requiring extraction into the formal kernel (see §7).
- **No changes to generation ID/hash/version contracts** beyond consuming them as-is.

## 3. Pluggable interface architecture

All freshness logic — registration, debounce/batching, reconciliation, backoff, and status — lives in a **client-agnostic Rust core** with no client-specific code. MCP tools, daemon HTTP routes, and any harness plugin are **thin shims**: they translate requests/results to universal JSON and call the same core dispatcher methods. No client-specific workaround exists in core; any future client-specific hook belongs in `plugins/<client>/` and may only expose the core status/control surface (never its own watcher or freshness logic).

This matches the Universal Core + Client Shims and AgentKernel patterns already used in NeuroStrata:

- **Single authority / one dispatcher:** the daemon hosts the core watch service (LadybugDB remains locked by the single HTTP daemon). MCP handlers, daemon HTTP endpoints, and CLI paths all funnel through the same core methods (one dispatcher). CLI interacts via daemon/MCP client surfaces rather than duplicating watch logic.
- **Ports/traits as seams; adapters at boundaries:** core depends on trait ports, not concrete clients, transports, stores, or filesystem backends. Concrete adapters are wired at the daemon boundary (composition root), following the existing trait-object seam style (e.g. `Arc<dyn VectorStore>`).
- **Fail-closed downcast invariant:** if any adapter must reach `LadybugStore`, it must use the private fail-closed downcast `crate::traits::VectorStore::as_any(store.as_ref()).downcast_ref::<LadybugStore>()` (never a local `AsAny` trait). This feature **prefers not to reach `LadybugStore` at all**; the watch core/ports must not require a LadybugDB downcast.
- **Explicit rejection:** client-specific watcher implementations (e.g. a plugin that owns or drives its own watcher) are rejected. Only the core owns watching, coalescing, persistence, reconciliation, and ingest triggering.

## 4. Architecture and component boundaries

### 4.1 Core module and trait seams (`src/ingest_watch.rs`)

`src/ingest_watch.rs` owns watcher state and decision logic. It is daemon-internal infrastructure (not a second ingest engine) and is structured around explicit ports so logic is reusable, testable, and transport/backend-agnostic.

**Core responsibilities (bounded):**

- Consume normalized events from a `WatchEventSource` and maintain per-root debounce/batch state.
- Apply flush/coalesce/backoff/dirty decisions via pure, testable functions.
- Filter candidate paths through the shared ingest eligibility predicate (see §4.4).
- Read/write registrations through a `WatchRegistrationStore` port and expose registration CRUD/status as core methods.
- Request ingest execution through an `IngestTrigger` port for dirty roots; never depend on parser/embedder/vector-store types directly.
- Produce `watch_status` snapshots (roots, last trigger/result/backoff/dirty state).

**Trait seams (explicit):**

1. **`WatchEventSource`** — async stream/iterator of normalized watch events.

   - **Contract:** yields normalized events (create/modify/remove/rename as path-bearing events) for watched roots; supports clean shutdown.
   - **Concrete adapter (core):** `notify`-backed adapter wrapping `notify::RecommendedWatcher`, translating native events to the normalized internal event type and forwarding watch add/remove for registered roots.
   - **Injectable test/manual adapter:** an in-process adapter (e.g. channel/stream fed by tests) implementing the same trait so unit tests inject synthetic events without real OS filesystem events. The spec’s “injectable event source so unit tests need no real FS events” requirement is an explicit consequence of this trait, not an ad-hoc hook.

2. **`WatchRegistrationStore`** — port for `load`, `save`, `list`, `upsert/register`, and `remove` of `{namespace, canonical_path}` registrations.

   - **Contract:** strict-JSON persistence, atomic owner-only writes, deterministic read/write semantics, and **fail-closed validation** on load (see §5). Not coupled to LadybugStore and not a database-backed port.
   - **Concrete adapter:** JSON-file adapter at `<config-dir>/watch_registrations.json` (daemon config directory). Malformed/invalid entries are rejected fail-closed with a traced reason and are never silently skipped into active state.

3. **`IngestTrigger`** — port the core calls to (re)ingest a canonical root for a namespace.
   - **Contract:** `trigger(namespace, canonical_path) -> TriggerResult/Progress` requests a full skip-fast ingest and reports completion/failure in terms the core can record (mapping to `IngestProgress` outcomes). Duplicate/same-dir in-flight behavior follows the underlying job semantics (attach, no overlap).
   - **Concrete adapter:** `IngestJobsTrigger` implemented by the existing `IngestJobs::run` seam (`ingests.run(namespace, dir, schema, embedder, vector_store)` with shipped schema), so the watcher core does not depend on parser, embedder, or vector-store types. This preserves the single ingest execution path.

**Ownership/lifecycle (follows `IngestJobs` pattern):** `start_daemon_on` wires concrete adapters and constructs `Arc<IngestWatchService>` parameterized/constructed over `WatchEventSource`, `WatchRegistrationStore`, and `IngestTrigger`, plus `startup_root`, `allowed_external_dirs`, and a shutdown signal. The service runs its event/flush loop as daemon background task(s) under graceful-shutdown discipline; `AppState` retains an `Arc` for handler/shim access. `IngestJobs` remains the owned ingest executor; watch-triggered work flows through it.

### 4.2 Watch registration model

A **registration** is exactly:

```json
{
  "namespace": "<string>",
  "canonical_path": "<absolute canonical directory path>"
}
```

- **Identity:** unique by `(namespace, canonical_path)` (a root is watched for a specific namespace).
- **Creation:** automatic on successful `neurostrata_ingest_directory` (after a successful run/attach result for that request), recording the ingested canonical directory and namespace. Idempotent: re-registering an existing pair is a no-op that may refresh status metadata but does not duplicate watcher state.
- **Removal:** explicit via `neurostrata_unwatch` (and daemon HTTP parity endpoint) for a `(namespace, canonical_path)` pair.
- **Status:** reported via `neurostrata_watch_status`.
- **Path authority (J6):** every registration, auto-registration, unwatch/removal, and registry load activation resolves/validates the path with `check_caller_path(raw_or_canonical, startup_root, allowed_external_dirs)` before mutating state or starting watches. Refusals return the same class of forbidden/path-policy error semantics as existing daemon path-guarded routes.

### 4.3 Interfaces and thin-shim surfaces

All surfaces call the same core dispatcher/service methods (no duplicated decision logic).

#### Core dispatcher methods (daemon-owned)

- `register(namespace, path_raw) -> Result<Registration, PathPolicyOrValidationError>`: J6-check, canonicalize, validate, persist via `WatchRegistrationStore`, ensure watched via `WatchEventSource` integration.
- `unregister(namespace, path_raw) -> Result<Removed, ...>`: J6-check, remove from registry/persistence, stop watching if unreferenced.
- `status(namespace_filter?) -> WatchStatusReport`: snapshot of all roots with runtime state.
- `on_ingest_success(namespace, canonical_path)`: idempotent auto-register hook invoked by ingest success paths after core ingest completion.

#### MCP tools (`src/server.rs`, thin shims)

1. **`neurostrata_unwatch`**

   - **Input (JSON):** `namespace` (string, required), `dir_path` (string, required; absolute or resolvable path to the watched root, matching `neurostrata_ingest_directory`).
   - **Behavior:** translates to core `unregister` (J6 enforced in core), returns `{ removed, namespace, canonical_path }` or an explicit forbidden/not-found/validation error. No watcher logic in the tool.

2. **`neurostrata_watch_status`**
   - **Input (JSON):** optional `namespace` filter.
   - **Behavior:** translates to core `status`, returns a read-only report (see §5 runtime fields). No side effects.

**Auto-registration hook:** `neurostrata_ingest_directory` (and the daemon `/ingest` success path) are thin shims that, on successful ingest result, invoke core `on_ingest_success` for the canonical directory. CLI, MCP, and daemon-triggered ingests therefore converge on identical registration behavior through the same core method.

#### Daemon HTTP parity (required thin shims)

Expose minimal authenticated, J6-consistent endpoints mirroring the tools so non-MCP clients share the same authority path:

- `POST /ingest/watch/unwatch` → core `unregister`.
- `GET /ingest/watch/status` → core `status`.

Both endpoints are singular and discoverable, are covered by the same auth middleware as other mutating/read routes, and contain no independent logic.

### 4.4 Shared eligibility predicate

Filesystem events and directory walks must agree on what is eligible, to avoid watch churn on ignored paths and to prevent indexing paths the walker would skip.

**Contract:** extract a shared, pure predicate `is_ingest_eligible_path(path, root)` used by both:

- the ingest walker/producer filter (replacing or delegating from the current inline `skipped_dirs` / skip logic in `src/parser/ingest.rs`), and
- the watcher event filter in `src/ingest_watch.rs`.

**Eligibility rules (concrete):** reject any path whose traversal is inside, or any component/name matches, the ingest skip set (currently including at least: `node_modules`, `target`, `vendor`, `.venv`, `venv`, `env`, `.env`, `dist`, `build`, `out`, `.dolt`, `.git`, `.next`, `.nuxt`, `__pycache__`, `.fastembed_cache`, `.idea`, `.vscode`, `coverage`), and reject LadybugDB/database files and transient store artifacts (e.g. `*.db`, `*.wal`, lock/checkpoint sidecars) anywhere under a registered root so the store does not trigger self-ingest loops. Accept only regular file/dir change events that would be considered by a normal ingest walk. The predicate is deterministic and side-effect free.

## 5. Persistence format (strict JSON)

**Location:** `<config-dir>/watch_registrations.json`, where `<config-dir>` is `Config::default_path()?.parent()` (same directory as `config.json`, i.e. `~/.config/neurostrata/` by default). Written atomically with owner-only permissions, matching `Config::save_to` discipline. The format is strict JSON only (no YAML/TOML).

**Top-level schema (versioned, strict fields):**

```json
{
  "schema_version": 1,
  "registrations": [
    {
      "namespace": "NeuroStrata",
      "canonical_path": "/abs/path/to/project",
      "created_at_unix": 1763596800,
      "updated_at_unix": 1763596800
    }
  ]
}
```

**JSON Schema (normative):**

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "title": "NeuroStrataWatchRegistrationsV1",
  "type": "object",
  "additionalProperties": false,
  "required": ["schema_version", "registrations"],
  "properties": {
    "schema_version": { "const": 1 },
    "registrations": {
      "type": "array",
      "items": {
        "type": "object",
        "additionalProperties": false,
        "required": [
          "namespace",
          "canonical_path",
          "created_at_unix",
          "updated_at_unix"
        ],
        "properties": {
          "namespace": { "type": "string", "minLength": 1 },
          "canonical_path": { "type": "string", "minLength": 1 },
          "created_at_unix": { "type": "integer", "minimum": 0 },
          "updated_at_unix": { "type": "integer", "minimum": 0 }
        }
      }
    }
  }
}
```

Absolute-path and traversal validation for `canonical_path` is intentionally not expressed as a regex (POSIX and Windows paths differ); it is enforced at runtime by `check_caller_path` (J6), which is platform-correct and handles symlink/`..` traversal.

**Runtime-only status fields** (not persisted, reported by `watch_status`): `dirty` (bool), `pending_paths_count` (int, bounded/capped in reports), `last_event_unix`, `last_trigger_unix`, `last_ingest_state` (`running|finished|failed` + error summary), `last_ingest_finished_unix`, `files_ingested`, `files_skipped`, `symbols_ingested`, `relinked_edges` (mirroring `IngestProgress` where applicable), `backoff_attempt`, `next_retry_unix`, `watch_active` (bool).

**Validation and fail-closed load semantics (normative):** the concrete `WatchRegistrationStore` (JSON-file adapter) validates document shape and every entry on load.

- **Malformed/invalid entries:** rejected **fail-closed** with a traced reason and **never silently skipped** into active state. Invalid entries are not activated/watched and are not treated as successful registrations.
- **J6 at load time:** every persisted path is re-checked via `check_caller_path` against `startup_root` and `allowed_external_dirs` before activation; a path-policy failure is a fail-closed rejection with a traced reason (no activation).
- **De-duplication:** duplicate `(namespace, canonical_path)` entries are detected; the registry’s effective active set is deterministic (only valid, authorized, unique entries activate) and load reports/traces any rejected duplicates or conflicts.
- **Corrupt/unreadable registry file:** the store fails closed for activation (loads no registrations, traces the error) and the daemon continues startup; mutating operations that cannot persist fail explicitly with an error (no silent success).
- **Missing file:** treated as an empty registry.

## 6. Data flow

### 6.1 Event to ingest

1. **Native events:** the concrete `WatchEventSource` (`notify::RecommendedWatcher` adapter) emits normalized create/modify/remove/rename events for watched roots.
2. **Normalize & filter:** core receives normalized events carrying absolute paths; drops events whose paths are ineligible per the shared eligibility predicate or outside any registered root.
3. **Per-root coalesce:** core inserts affected eligible paths into the root’s coalescing `HashSet<PathBuf>` (bounded bookkeeping; per-root). `rename` is normalized to `remove(old)+create(new)` contributions.
4. **Debounce:** 300 ms quiet-period timer per root; 2 s max window from first event in a burst to prevent starvation under continuous change.
5. **Flush:** on flush, if the root is not already mid-run, core marks a trigger attempt (clear/hand off the pending set for this cycle, set `last_trigger`) and requests ingest via the **`IngestTrigger` port**. The concrete `IngestJobsTrigger` adapter fulfills this by invoking `IngestJobs::run(namespace, canonical_path, schema, embedder, vector_store)` with the shipped schema, preserving existing job semantics.
6. **Progress/result:** core observes completion via trigger/`IngestProgress` outcomes and updates `last_ingest_*` status. Because unchanged trees short-circuit cheaply and SHA-256 skip/carry-forward apply, a triggered full skip-fast run is safe and efficient even when many files are unaffected.

### 6.2 Concurrency, duplicates, and dirty tracking

- **Single active walk per namespace/root semantics:** the `IngestTrigger`/`IngestJobs` path attaches same-dir callers to a running job and refuses a different directory for the same namespace. The watcher core must not create parallel walks.
- **Trigger while running:** if a flush occurs while a run for that root is active (or attach indicates running), core marks the root `dirty` and retains/accumulates coalesced paths; it does not start a second run. On completion, if `dirty` or backoff has elapsed, core schedules/requests the next full skip-fast ingest via `IngestTrigger` so all changes since the previous snapshot are reconciled. This guarantees convergence without overlapping ingest execution.
- **Per-root namespace isolation:** each registration carries its own namespace; scheduling and state are tracked per `(namespace, canonical_path)`.

### 6.3 Startup reconciliation and shutdown

- **Boot:** load registrations through `WatchRegistrationStore` (fail-closed validation + J6 re-check), register/activate allowed roots with `WatchEventSource`, then enqueue one full skip-fast ingest per activated root via `IngestTrigger` (reconciliation for events missed while the daemon was down). Runs flow through `IngestJobs`, so normal namespace conflict rules apply and are reported in status.
- **Shutdown:** graceful shutdown continues to call `IngestJobs::wait_idle(limit)`; because watch-triggered reconciliations/flushes use `IngestJobs` via `IngestTrigger`, they are covered by existing wait logic. The watcher stops accepting new flushes once shutdown begins and persists no transient runtime-only state beyond the durable registry.

## 7. Error handling and failure modes

| Condition                                                    | Behavior                                                                                                                                                                                                                                      |
| ------------------------------------------------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Watcher backend/event stream error                           | Trace error, keep daemon alive, mark affected root `watch_active=false`/degraded in status, attempt reinitialize with bounded backoff (1 s→30 s policy) without crashing.                                                                     |
| Ingest run failure                                           | Root remains `dirty` (work pending), record error in `last_ingest_*`, apply exponential backoff **1 s → 2 s → 4 s → 8 s → 16 s → 30 s** (cap) before retry; reset backoff on success. Never crash the daemon.                                 |
| Path policy refusal (J6) on register/unwatch/load activation | Refuse the mutating operation; on load, reject the entry fail-closed with a traced reason (no activation). Never silently skip a malformed/unauthorized entry into active state. Daemon continues.                                            |
| Namespace conflict (different dir ingesting same namespace)  | Surface the existing `IngestJobs` refusal in status/trigger result; root marked failed/degraded with actionable error; no concurrent walk started.                                                                                            |
| Persistence read/write/validation failure                    | Fail the mutating operation with an explicit error (no silent success). On boot, unreadable/corrupt registry fails closed for activation (no registrations activated), traces the error, and daemon continues. Missing file ⇒ empty registry. |
| Overflow/continuous event storms                             | Max 2 s flush window bounds latency; coalescing collapses path sets; ineligible paths dropped early.                                                                                                                                          |

All watcher/trigger/retry transitions are traced (structured logs) for operator diagnosis.

## 8. Security and formal-verification posture

- **J6 caller-path authority:** `check_caller_path` is mandatory for every path accepted into the watch registry (manual register, auto-register on ingest success, unwatch/removal), for every persisted entry at load/activation time, and for any daemon route that resolves a watch path. Paths must resolve inside `startup_root` or `allowed_external_dirs`; symlink/`..` traversal handling follows `check_caller_path` semantics.
- **No privilege escalation:** watcher-triggered ingests run with the same daemon identity, schema, and store access as explicit ingest requests through the existing `IngestJobs` path.
- **Secret/eligibility safety:** unchanged ingest pipeline behavior (secret scan, eligibility/skip rules) applies; DB/store artifacts are excluded from watch eligibility to prevent feedback loops.
- **Port isolation:** the watch core does not depend on LadybugStore or parser/embedder/vector-store types; if a future adapter must reach `LadybugStore`, it must use the private fail-closed downcast `crate::traits::VectorStore::as_any(store.as_ref()).downcast_ref::<LadybugStore>()` (never a local `AsAny` trait). This design avoids that downcast entirely.
- **Client-agnostic core:** no client-specific watcher or workaround in core; plugins are status/control shims only (see §3).
- **Formal-verification reflex (explicit):** this feature contains debounce/coalescing/backoff/flush decisions but **no gate, ACL, or consensus invariant**. Therefore **no `src/formal_kernel.rs` change** is required or introduced. Pure decision logic is extracted as testable functions (see §9) to make behavior auditable and deterministic.

## 9. Testing strategy and verification

**Lane rule (binding):** focused tests + `cargo check` only. No full-workspace `cargo test`. No sleep-polling in tests.

### 9.1 Unit tests (injectable `WatchEventSource` / pure functions, no real FS events)

Driven through the `WatchEventSource` trait (injectable test/manual adapter) and pure decision functions — no ad-hoc test hooks and no real OS filesystem events:

- **Debounce coalescing:** multiple events within 300 ms quiet period produce a single flush with a coalesced path set.
- **Max-window flush:** continuous events force a flush at the 2 s max window (starvation prevention).
- **Eligibility filtering:** ineligible paths/components (skip dirs, `.git`, build artifacts, DB/WAL/lock artifacts) are dropped; eligible source changes pass; shared predicate parity is asserted against the ingest walker’s expectations.
- **Rename handling:** rename normalized to remove+create effects in the coalesced set.
- **Registration persistence round-trip and fail-closed validation:** serialize/deserialize v1 strict JSON, de-duplication, atomic write/read behavior, and **fail-closed rejection with traced reason** (never silently skipped into active state) for malformed entries, schema violations, and entries failing J6 re-check on load.
- **Backoff transitions:** 1s→30s exponential progression, cap, and reset on success.
- **Dirty-during-run behavior:** flush while active marks dirty and schedules follow-up after completion via `IngestTrigger` (no overlapping runs).

### 9.2 Integration test (one real watcher)

- One integration test in a temp directory using the real `RecommendedWatcher` adapter: create/modify/delete/rename an eligible file under a registered root and assert the coalesced flush triggers an ingest run through `IngestJobs`/`IngestTrigger` (observable via progress/status), while ineligible paths do not trigger.

### 9.3 Local verification commands (focused)

- `cargo check`
- Focused `cargo test` invocations limited to `ingest_watch` unit tests and the single real-watcher integration test target/module(s) introduced by this change, scoped via explicit test-name filters limited to modules introduced by this change.

## 10. Files/components to add or modify

| Path                                              | Change                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            |
| ------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `Cargo.toml`                                      | Add filesystem-watch dependency providing `notify::RecommendedWatcher` (and required features) for the concrete `WatchEventSource` adapter.                                                                                                                                                                                                                                                                                                                                                                       |
| `src/ingest_watch.rs` (**new**)                   | Client-agnostic core: trait ports `WatchEventSource`, `WatchRegistrationStore`, `IngestTrigger`; normalized event model; pure debounce/coalesce/flush/backoff/dirty decision functions; concrete `notify`-backed and injectable test `WatchEventSource` adapters; concrete strict-JSON `WatchRegistrationStore` adapter (fail-closed validation, J6 re-check on load, atomic owner-only writes); concrete `IngestJobsTrigger` adapter; per-root state, reconciliation, status reporting. No client-specific code. |
| `src/daemon.rs`                                   | Composition root: wire concrete adapters and construct/start `IngestWatchService` in `start_daemon_on`; add to `AppState`; wire startup reconciliation and graceful-shutdown coordination; add J6-guarded thin-shim HTTP parity endpoints for status/unwatch; invoke core auto-register hook on successful `/ingest`.                                                                                                                                                                                             |
| `src/server.rs`                                   | Add thin-shim MCP tools `neurostrata_unwatch` and `neurostrata_watch_status` calling core dispatcher methods; invoke core auto-register on successful `neurostrata_ingest_directory`. No duplicated logic.                                                                                                                                                                                                                                                                                                        |
| `src/parser/ingest.rs`                            | Extract/expose the shared eligibility predicate used by the walker and consumed by the watcher so filtering is defined once; no change to generation/ingest semantics.                                                                                                                                                                                                                                                                                                                                            |
| `src/config.rs` (or colocated persistence helper) | Config-dir path resolution/helpers consistent with existing config storage conventions (atomic, restrictive writes) for the JSON-file registration store.                                                                                                                                                                                                                                                                                                                                                         |
| `src/main.rs` / module wiring                     | Declare `mod ingest_watch;` and wire daemon/CLI plumbing required by the new surfaces (CLI remains a thin client of daemon/MCP surfaces, no duplicated watch logic).                                                                                                                                                                                                                                                                                                                                              |
| Tests (new/updated, focused)                      | `src/ingest_watch.rs` unit tests using injectable `WatchEventSource`, one real-watcher integration test in a temp dir, and narrowly scoped updates for the extracted eligibility predicate and fail-closed persistence validation.                                                                                                                                                                                                                                                                                |

## 11. Acceptance criteria (observable and testable)

1. **Pluggable core + thin shims:** `WatchEventSource`, `WatchRegistrationStore`, and `IngestTrigger` ports exist; core watch logic is client-agnostic; MCP/daemon HTTP (and CLI via daemon surfaces) are thin shims calling the same core dispatcher methods. No client-specific watcher implementation and no LadybugStore coupling in core.
2. **Automatic registration:** a successful `neurostrata_ingest_directory` (MCP or daemon `/ingest`) registers `(namespace, canonical_path)` idempotently via core and persists it to `<config-dir>/watch_registrations.json`.
3. **Event-driven freshness:** an eligible create/modify/delete/rename under a registered root triggers a debounced (300 ms quiet, 2 s max window) coalesced ingest via `IngestTrigger`/`IngestJobs::run`; the resulting graph/state reflects the change without manual re-ingest.
4. **No parallel path:** watcher-triggered updates occur only through the existing generation ingest path; there is no partial/direct AST mutation path.
5. **Concurrency safety:** events arriving during an active run do not start overlapping runs; the root becomes/remains dirty and a follow-up skip-fast ingest runs after completion.
6. **Reconciliation:** restarting the daemon reloads registrations (fail-closed validation + J6 re-check) and performs one full skip-fast ingest per activated root, converging state even if events were missed.
7. **Failure resilience:** a failed ingest leaves the root dirty, retries with exponential backoff 1s→30s, emits traces, and never crashes the daemon.
8. **Control & visibility:** `neurostrata_unwatch` removes a registration (persisted) and `neurostrata_watch_status` reports roots with last trigger/result/backoff/dirty state; parity HTTP endpoints exist as thin shims.
9. **Security and persistence correctness:** all register/unregister/status-by-path operations and persisted-entry activation enforce `check_caller_path` (J6); malformed/invalid/unauthorized persisted entries are rejected fail-closed with traced reasons and never silently skipped into active state; registry format is strict JSON only.
10. **Eligibility parity:** watcher ignores the same paths/classes the ingest walker ignores (including `.git`, build artifacts, and DB/store files), preventing self-trigger loops and irrelevant churn.
11. **Verification:** focused unit tests (injectable `WatchEventSource`, coalesce, max window, eligibility, rename, persistence round-trip and fail-closed validation, backoff, dirty-during-run) and one real-watcher temp-dir integration test pass, and `cargo check` succeeds under the focused-test lane rule.

## 12. Risks and trade-offs (explicit decisions)

### Accepted: event-batched triggers into the existing generation ingest

- **Rationale:** `ingest_directory` already provides SHA-256 skip, unchanged-node carry-forward, whole-tree unchanged short-circuit, version gating, secret scanning, and atomic generation publication. Triggering it on coalesced batches via `IngestTrigger`→`IngestJobs` yields correct, convergent freshness with minimal new machinery and no duplicate consistency logic.
- **Trade-off:** a burst triggers a (skip-fast) full re-walk of the root rather than a micro-scoped parse of only changed files. This is acceptable because unchanged work is cheap by construction and correctness/convergence are prioritized.

### Rejected: parallel partial ingest / direct incremental AST mutation path

- **Why rejected:** maintaining a second, event-scoped mutation path alongside generation ingest would fork identity, carry-forward, deletion, dependency, and publication semantics, reintroducing stale/misaligned graph states and substantially increasing correctness risk. K3 refinement (accepted): feed batches into the existing generation ingest; do not build a parallel AST-update path.

### Rejected: polling-based freshness

- **Why rejected:** polling adds latency/cost trade-offs, redundant scans under idle periods, and still misses the desired event-driven convergence model. Native filesystem events plus debounced batching and startup reconciliation provide timely, efficient freshness without periodic rescans.

### Rejected: client-specific watcher implementations

- **Why rejected:** violates Universal Core + Client Shims; would duplicate watcher/freshness logic outside the core, fragment behavior across clients/plugins, and bypass the single daemon-owned dispatcher. Plugins may expose only the core status/control surface.

### Noted considerations

- **Watcher backend variability:** `RecommendedWatcher` behavior differs by platform; the `WatchEventSource` port with injectable and real adapters bounds this risk, while status/backoff keep failures observable and recoverable.
- **Namespace contention:** existing `IngestJobs` correctly refuses conflicting directories per namespace; the watcher surfaces this rather than bypassing it.

## 13. Delivery and review gate

This document is a design/spec artifact only. Implementation work must satisfy the owner Prime Directive (no deferral), the focused-test verification lane, the pluggable interface architecture (ports/adapters, thin shims, client-agnostic core), and the governing project rules. **No pull request is published until implementation, documentation, focused tests, local verification, and an independent review are all clean.** No implementation code is part of this deliverable.
