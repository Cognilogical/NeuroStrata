# 📓 NeuroStrata Changelog

All notable changes to the NeuroStrata project will be documented in this file.

---

## [Unreleased]

### Added
- **Task rename surface:** `neurostrata_task_update` accepts `title` — it rewrites the task's `content`, re-embeds it (so `search_memory` matches the new wording), and appends `renamed: 'old' -> 'new'` to the task's history. Titles are validated identically on create and rename (non-empty, max 200 characters); the reply gains a `renamed` field. Closes the vocabulary-scrub gap that left stale engine names in live task titles.

## [1.7.0] - 2026-10-08

### Added
- **Central Executive (task subsystem):** `neurostrata_task_create|claim|update|list|complete|validate` plus `neurostrata_bootstrap` and `neurostrata_task_setup` instructor tools. Goals are LadybugDB memories (`memory_type: "task"`) driven by a hand-rolled 4-state / 8-edge machine; `done` is reachable only through `neurostrata_task_complete`, which requires a consolidated extraction edge (`EXTRACTED_FROM`).
- **Supervisory Attentional System:** `neurostrata-mcp task gate|validate` and `hooks install` — one pre-push gate hook (exit-code enforcement) with a daemon `/tasks/gate` route and a direct-store fallback. No external task binary required.
- **Beads migration:** `neurostrata-mcp task import <namespace> --from-beads <path>` — one-shot and idempotent on `bead_id`; closed beads imported as `done` + `grandfathered` past the extraction gate.
- **Episodic Buffer writer:** `neurostrata_append_log` — timestamped session entries with `### 🔄 Topic Switch` markers, 500KB rollover, retention pruning (`buffer_retention_days`), `episodic_buffer: false` disable, and secret rejection on entry.
- **Memory vocabulary v2:** `task` memory type and the `EXTRACTED_FROM` relation.
- **Wiring panel in `bootstrap` / `task_setup`:** the full gate registry — automatic gates vs reminders, `runs_in` (`core` | `git-pre-push` | `project-pipeline` | `agent-reminder`), `coverage.uncovered`, and a `verified` block whose confirmed items are subtracted from `instructions`. Git-first, never git-only: one instruction template names git the default instance and tells any project (CMS, media, research) to identify its own chokepoint, wire `task gate --strict` into it, and record the gate point as a rule memory.
- **Rule honesty (vocabulary v3):** `memory_type: rule` gains `enforcement` (`ENFORCED`/`PARTIAL`/`NOT_ENFORCED`), `source`, and `guard`; `task_validate` fails ENFORCED-without-guard (`rule_overclaims_enforcement`).

### Changed
- **Documentation scrub:** Executive-suite naming adopted (Central Executive, Goal, Supervisory Attentional System, Knowledge Consolidation, Action Initiation, Working Memory) across README, AGENTS.md, and the cognitive-architecture docs; CLI-readme documents the new subcommands; changelog backfilled for 1.4.0–1.6.0.
- **AGENTS.md workflow:** §1/§2 moved from `bd` commands to the task tools; the soft locks now describe the gate and the done-funnel.

### Removed
- **Beads (`bd`) dependency:** the `.beads/` store, hooks, and Dolt sync are retired — task state lives in LadybugDB and never touches the repository.

### Fixed
- **Backup no longer crashes (data safety):** `neurostrata-mcp backup` segfaulted both with and without a daemon — lbug 0.20.4's planner crashes inside `EXPORT DATABASE` on *every* store (gdb: `planExportTableData` → `std::__format` on a dangling string_view). Backups are now checkpointed file snapshots (`ladybug.store` + `manifest.json`), `restore` copies the snapshot before any engine opens it, and the daemon survives `/backup`. Verified round-trip on a live 8-namespace store.
- **`neurostrata-mcp status`:** read-only preflight (store, daemon, lock) with distinct exit codes for healthy / down / busy — the missing check that made daemon pile-ups possible. A second daemon is refused at the lock with a legible message (verified).
- **Migration, graph export, and read-only tooling no longer demand a daemon shutdown:** `task import`, `export-graph`, and the read-only trio (`doctor`, `list`, `namespaces`) route through the running daemon (`/tasks/import`, `/graph?all=true`, `/cli/read`) — the lock gates writers, not readers. Daemon-failure errors now lead with `run status — do NOT start a daemon`.
- **`task_setup` suggests rules worth trusting:** language detection weighs counted source files, not manifest presence (a tooling `package.json` no longer outvotes a Go-majority tree); every suggestion is flagged `heuristic: true` / `verified: false`, carries its `similar_existing` memories, and real overlap lands in `conflicts[]`.
- **`export-graph` export fidelity:** nodes now carry `metadata` and a `superseded` marker with `superseded_by`; `EXTRACTED_FROM` consolidation edges are exported alongside `RELATES_TO`/`CONTAINS`/`GOVERNS`; `--exclude-superseded` drops retired rows entirely.
- **`doctor` is scoped and labeled (round 2):** `--namespace <ns>` (refuses unknown names) or all namespaces in deterministic order, with every finding line carrying its `[namespace]` label — an unqualified health line once produced a confident wrong answer about a different project.
- **`supersede_memory` repairs structural metadata (round 2):** accepts `governs`/`related_to`/`contained_by` and replaces them wholesale; anything omitted carries over unchanged and is **named in the reply** — a correction can no longer silently leave a stale pointer.
- **Rules carry provenance (round 2):** `task_validate` fails rules with no `metadata.source` (`rule_without_source`); `source` accepts a dated string or `{kind, ref, captured_at}`.
- **Runtime skew detection (round 3):** the daemon serves its build identity on `GET /info` (fingerprint captured at startup); `status` compares it against the binary on disk and exits `3` on a mismatch — a fix "missing" from a stale daemon is never again mistaken for a defect. Root cause of the round-3 false "still open".
- **`supersede_memory` round 3:** accepts `locations` (`add_memory` parity — replaces `refs` wholesale and re-derives `governs`); an unambiguous `Governs:` line in the corrected prose is **honoured** when no parameter speaks (the line is no longer written-but-ignored); conflicting lines are refused, not guessed; the reply names which source set the pointer (parameter | locations | content line | inherited).
- **Self-contradicting records marked (round 3):** when a memory's prose `Governs:` line disagrees with its authoritative metadata, the rendered record carries a `NOTE` naming both — two contradictory lists never render unmarked again.

---

## [1.6.0] - 2026-10-08

### Added
- **Prefrontal Cortex (guard module):** behavioral constraint validation for state-mutating actions — semantic rule retrieval from LadybugDB, deterministic rejection, and an optional ephemeral Podman dry-run (`--network=none`, read-only mount, 5s timeout). Exposed as `neurocortex_local_guard_validate` / `neurocortex_learn_behavioral_rule` and the daemon `/validate` route.
- **Dendritic Bridge:** `RemoteEmbedder` + `build_embedder()` — OpenAI-compatible embedding endpoints declared in `~/.config/neurostrata/embedders.json`, degrading to the local fastembed dendrite when no bridge is declared.
- **Judgment deduplication:** TypeSafe Jev model integration with a circuit breaker for memory dedup checking.

### Changed
- README rewritten around the cognitive-architecture naming (SynapticGraph, Engram, Tri-Strata Model, Episodic Buffer, Prefrontal Cortex, Dendritic Bridge).

### Fixed
- Deleting an absent memory fails loudly instead of returning no-op success.

---

## [1.5.0] - 2026-09-19

### Added
- **Path-labeled retrieval evidence:** search returns transient evidence paths (`Path [kind]: ...` / `Why: ...`) from bounded directed graph queries; memory vocabulary v1 (`RELATES_TO`, `CONTAINS`, `GOVERNS`).

### Fixed
- MCP `serverInfo` reports the crate version instead of a hardcoded `1.0.0`.
- Windows release builds carry the OpenSSL environment setup ported from CI.

---

## [1.4.0] - 2026-09-19

### Added
- **Doctor & migration toolchain (PRs #21–#28):** repository-relative node identity with `absolute_path` metadata, truthful MCP answers (`get_memory`), GUI daemon client groundwork, ingest-as-a-task background jobs, additive memory correction, and `neurostrata-mcp doctor` for upgrade leftovers.

### Changed
- Dependency upgrades: tauri 2.11.5 + plugins, root crate to latest, lbug 0.15.3 → 0.20.4, web-ui npm dependencies.

### Fixed
- Fallible response encoding; secret scanning on MCP entry points; supersede hardening.
- Store: bounded usage bias, active-result refill, active-only graph export.
- Ingest keeps blocking filesystem and inference work off the async runtime.
- Windows: vendored OpenSSL (Strawberry Perl) for lbug link directives; rustls TLS for fastembed/ort prebuilt downloads; intel-mac CI target dropped; CI runs the test suite, not just the build.
- LanceDB/Qdrant-era leftovers removed; web-ui `no-explicit-any` lint debt cleared.

---

## [1.3.0] - 2026-05-25

### Added
- **Clap Subcommand CLI Integration**: Integrated `clap` v4 with derive-style command parsing to cleanly structure vector store CLI operations (`daemon`, `namespaces`, `list`, `ingest`, `export-graph`, `delete`, `add`, `edit`).
- **External Plugin Fallback**: Implemented robust prefix matching and cross-platform child subprocess spawning for unrecognized commands, ensuring 100% backward compatibility for external plugin runners.
- **GitHub Issue Synchronizer**: Created a standalone `sync_github_issues.sh` utility to sync outstanding tasks and post comments, with a smart offline/unauthenticated fallback to local markdown files under `docs/github_issues/`.
- **Dolt Beads Tracking**: Created and completed issue tracking beads for Phases 4-8 in the local beads Dolt database.

---

## [1.2.0] - 2026-05-25

### Added
- **Regex Secret Scrubber**: Implemented robust regex-based secret scanning prior to DB insertion.
- **Extracted AST Schema**: Separated AST schemas out to a declarative `schema.json` file for cleaner maintainability.

### Changed
- **Logarithmic Neural Gain**: Replaced linear boost in semantic search with logarithmic scaling to prevent query saturation blindness.
- **Subprocess Spawning**: Transitioned to cross-platform `.status()` execution over Unix-only `.exec()` hijacking.
- **Decoupled Handlers**: Completely refactored server route handling into independent controller functions.
- **Model Configuration Support**: Allowed FastEmbed models to be dynamically instantiated via the `NEUROSTRATA_MODEL` env var.

### Fixed
- **Graph Inlining Defect**: Removed context neighborhood concatenation on direct fetches to solve permanent visualization corruption during DB moves.
- **Ingestor Exclusion**: Corrected extension filter omitting structural graph processing on unspecified languages.

---

## [1.1.1] - 2026-05-25

### Added
- **Asynchronous Neural Gain Updates:** Every successful semantic query asynchronously increments the target memory's `access_count` in Kùzu Graph database in a non-blocking background tokio task.
- **Bi-Temporal Validation Logic:** Automatic runtime filtering of expired memories whose `valid_to` Unix timestamp has passed, avoiding outdated context injection.
- **Cypher-Injection Hardening:** Added robust escaping mechanisms for inputs (`escape_kuzu_string`) to neutralize trailing-backslash (`\\`) and single-quote (`\'`) Cypher-injection vectors.

### Changed
- **Single-Pass AST & Text Walk:** Replaced the legacy double-walker implementation with a single unified, highly optimized directory walker loop to ingest files and AST structures simultaneously, dramatically cutting ingestion times.
- **Improved MCP Error Responses:** Replaced raw `unwrap`s in JSON-RPC serialization with safe propagation, avoiding daemon crashes on invalid payload states.
- **Robust Path Resolution:** Upgraded path extraction in daemon and parser modules to safely handle systems with sparse files or specialized links.

### Fixed
- **Temporal Gate Logic Bug:** Resolved a regression where future-dated valid memories were incorrectly ignored under specific timezones.
- **Ladybug Store Access Counters:** Fixed missing `increment_access_count` trait implementations and integrated it into the main MCP server pipeline.
- **CLI Database Lock Conflict:** Added active check for running daemon port `34343` during CLI invocations, preventing simultaneous database write access crashes on locked Kùzu DB storage.
- **Dead Code Cleanup:** Pruned deprecated and unused mock structures (`start_mcp_server`, `generate_canvas`) to keep codebase clean and optimized.

---

## [1.0.0] - 2026-04-15

### Added
- **Rust Transition (Complete):** Core backend successfully migrated from Go to Rust for memory density, safety, and parallel search performance.
- **Kùzu Graph & Vector Engine:** Integrated local embedded Kùzu graph store, supporting semantic nodes, relations (`governs`, `relates_to`), and dense vector storage.
- **Tantivy FTS Hybrid Search:** Integrated hybrid exact-keyword and vector search using Reciprocal Rank Fusion (RRF).
- **Daemon Mode Proxying:** Implemented a persistent TCP daemon on port `34343` with a lightweight stdio proxy for fast, lock-free editor integration.
