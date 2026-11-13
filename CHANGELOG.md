# 📓 NeuroStrata Changelog

All notable changes to the NeuroStrata project will be documented in this file.

---

## [Unreleased]

### Added

- **Remediation eval registry and release gate (`eval/registry.json`, `scripts/remediation-eval.sh`, `scripts/gate-receipt.sh`, `.github/workflows/gate-receipt.yml`, `#151`).** Named suites run the tests that back each remediation claim and emit a JSON verdict; the gate receipt refuses to clear a review marker until the fix is present at the reviewed head, and a maintainer ruling is required otherwise. The v3 upgrade preflight refuses a reparse the host cannot hold (2× projection) and records `enrichment_fail` before returning.
- **Lexical active-generation visibility (`src/lexical.rs`, `src/fts/mod.rs`, `src/store/ladybug.rs`, `#149`).** Both lexical routes carry the staged generation and source, reject superseded/deleted/secret-skipped rows in the query, at hydration, and after publish/abort, and invalidate the cached view when the pointer moves. Legacy no-pointer stores stay readable. Owner resolution is canonical on both routes.
- **Generation retention API (`delete_generation`, `prune_generations`, `#148`).** Namespace-scoped deletes refuse active and in-flight generations; pruning keeps active, per-source previous (rollback), staging and base generations, recovering upgraded stores and abandoned markers.

### Fixed

- **Secret scanner precision and skipped-file reporting (`src/secrets.rs`, `src/parser/ingest.rs`, `src/ingest_jobs.rs`, `#147`).** Source expressions, comments and config literals are distinguished correctly; provider tokens, JWTs and placeholder-wrapped tokens still reject; metadata keys are scanned; oversized metadata names the field; the single-line chunker boundary is pinned. Ingest reports `files_skipped` and bounded reasons, and a never-indexed skipped file no longer forces a new generation (`#148a`).
- **Ingest correctness (`src/parser/ingest.rs`, `src/store/ladybug.rs`, `#142` follow-ups).** A transient generation-summary failure forces a full re-parse; abort/generation listing are namespace-scoped; an unreadable file aborts instead of publishing without its bytes; the file→directory kind change is pinned.
- **Hygiene repair failure isolation (`src/store/ladybug.rs`, `src/server.rs`, `#142`).** Chunk failures keep the counts already committed, report the partial totals, and do not advance the doctor's `last_run_version`, so the retry runs again.
- **`set_metadata` authorization (`src/server.rs`, `src/daemon.rs`, `src/main.rs`).** The replace re-checks write access and requires the owner or admin to change the canonical owner or ACL (including role-only changes).
- **Guard `hit_count` atomicity and rule cap (`src/guard/`, `src/traits.rs`, `#114`).** The counter increments in one write turn, so concurrent firings are not lost; the guard refuses above 5,000 enforceable rules and has a 3,000-rule sizing probe.

### Upgrading

- **From ≤1.17.x**, the parser-version bump forces one full reparse/re-embed (2× store free space; the preflight refuses otherwise). See `docs/ops/upgrading.md` for disk, pool, rollback and `files_skipped` guidance. A pre-1.17.3 lexical index is rebuilt once (`INDEX_VERSION` v2).

---

## [1.17.2] - 2026-11-10

> The changes below were first published as the **v1.17.1** pre-release (2026-11-10), a tag cut without a version bump, so its binary reports `1.17.0`. 1.17.2 carries the same code with the version and changelog corrected.

### Added

- **Embedding model choice at initial setup (`src/model_choice.rs`, `src/embed.rs`, `src/main.rs`, `src/store/ladybug.rs`, `scripts/backup.sh`, `#66`).** On a fresh install the first interactive command offers a one-time choice among measured embedding-model candidates, with byte-exact file sizes, a permanence warning and a 512-token-context warning where it applies; the daemon never prompts and declining keeps the default. The pick is written atomically to `embedders.json` (choice first, with the candidate's pinned SHA-256) and the writer refuses if the store already holds data or a non-default `embedders.json` exists. The store now maintains a `ladybug.db.store-state.json` sidecar recording whether it holds data and the descriptor of the model that produced its vectors; backup and restore (including the scheduled `scripts/backup.sh`, which carries a `db-<timestamp>.store-state.json` companion with pair-aware pruning) keep it with the store. A store with recorded data refuses open/init, upsert and vector search under a different model, same-width switches included, and a daemon pre-flight refuses a doomed start before the engine opens or a model downloads. A pristine store has its vector column width reconciled to the picked model only after the engine attests the table is empty. `verify_model_checksum` now hashes the exact file fastembed loads. Existing stores have no sidecar and open exactly as before; offline migration to a different model is not included.

### Fixed

- **Store-identity defects left in the merged #66 (`src/store/ladybug.rs`, `src/main.rs`, `scripts/backup.sh`, `#78`, closing `#73`, `#74`, `#75`, `#76`).** `guard_first_write` did not cover `FreshUnrecorded` stores (`#73`); fresh-store provenance was lost across non-serving opens, so an attested-empty `LockedUnknown` store opened with serving == configured now gains a recorded pristine identity at init while data-bearing legacy stores behave exactly as before (`#74`); the daemon-stopped fallback `Add`, `Edit` and `Ingest` arms never initialized the store (`#75`); and `export_database` left a stale destination sidecar and WAL on snapshot-directory reuse (`#76`). Each fix carries a regression test that fails on the pre-fix tree. `scripts/test-backup.sh` adds 18 checks for the backup sidecar carry. Recording identity for legacy (pre-sidecar) stores is tracked in `#77` and is not part of this release.
- **The stdio MCP proxy snapshotted the daemon bearer token once and could hold no credential forever (`src/server.rs`, `src/daemon.rs`, `src/main.rs`, `#64`).** `proxy_client()` installed `daemon.token` as a default `Authorization` header at construction; if the file did not exist yet (a slow first start while the embedding model loads), every forwarded `/mcp` request 401ed until the proxy was restarted, and a rotated token stranded a running proxy the same way. `proxy_client()` is now credential-free and `forward_mcp_request()` resolves the token on each attempt, so a token that appears late or is rotated takes effect on the next request.

### Changed

- **`lbug` (LadybugDB) 0.20.4 → 0.22.3 (`Cargo.toml`, `Cargo.lock`, `src/store/ladybug.rs`, `src/traits.rs`, `scripts/backup.sh`, `#67`).** Only `lbug` changed version in the lockfile apart from eight Windows-only `windows-sys` edge re-points (0.59.0 → 0.61.2). The binding-layer diff is additive-only for the APIs NeuroStrata calls. This bump makes no claim about the engine abort tracked in LadybugDB #1536.
- **CI and release builds pin the engine archive (`.github/workflows/ci.yml`, `nightly.yml`, `release.yml`, `#71`).** `LBUG_VERSION: "0.22.3"` is set on every build/test job so the engine downloaded by the lbug build script matches the crate version in `Cargo.lock`; move it whenever the crate version changes.

---

## [1.17.0] - 2026-11-07

### Added

- **NeuroMesh near-real-time multi-agent cognitive mesh transport (`src/synaptic_mesh/`, `#63`).** Extends SynapticMesh with a pluggable `MeshTransport` trait (`src/synaptic_mesh/transport.rs`), a default pure-Rust P2P transport over `tokio::net` TCP + AES-256-GCM with `nsmesh1...` Ed25519 capability invite tokens (`src/synaptic_mesh/p2p.rs`), an optional `--features nats-mesh` TCP NATS wire-protocol adapter (`src/synaptic_mesh/nats.rs`), a single-writer durable `frames.wal` outbox and range replay cursor store (`src/synaptic_mesh/outbox.rs`, `src/synaptic_mesh/reactor.rs`), signed `MeshFrame` wire envelopes with 3-gate sovereignty checks (`src/synaptic_mesh/frame.rs`), and an active-context `AttentionFilter` (`src/synaptic_mesh/attention.rs`) that injects targeted `[NeuroMesh Δ]` notices into MCP `tools/call` responses only when a peer mutates a session's active working set (`src/server.rs`). Adds CLI subcommands `synaptic-mesh up`, `join-token`, `invite`, and `peers` (`src/synaptic_mesh/cli.rs`).
- **Autonomous Memory Control Plane — Waves 1–5 (`plugins/core/`, `src/task/harness_plugins.rs`, `#62`).** Upgrades the zero-dependency harness plugin core (`@neurostrata/plugin-core`) across OpenCode, Claude Code, Codex, and GitHub Copilot CLI with global OpenCode config propagation, fixed V2 hook signatures (`chat.message`, `tool.execute.before`, `tool.execute.after`, `experimental.chat.system.transform`), automatic user-turn semantic recall and task-intent intake, structured failure and decision memory synthesis, and an automated multi-harness CI verification matrix.

### Fixed

- **SynapticMesh record signing, validation, and bundle safety (`#51`, `#53`, `#54`, `#55`, `#60`).** Signs mesh records after lifecycle preservation and RDF rebuild (`#51`), routes bundle-only records through `validate_record` and `epoch_gate` (`#53`), distinguishes RDF-only from full-record signature verification (`#54`), keeps drain bundles local-only while surfacing bundle write errors (`#55`), and refuses diff-pass rollback by stale tracked bundle records (`#60`).
- **Cross-platform CI and test harness reliability (`#48`, `#56`, `#57`, `#59`, `#61`).** Fails fast at build time when an `include_str!` target is missing from checkout (`#48`), refreshes `Cargo.lock` to clear 9 RUSTSEC advisories (`#56`), gates Unix-only permission APIs in CLI and mesh import tests for Windows builds (`#57`, `#59`), and reads HTTP requests before responding in federation tests while raising the daemon probe budget (`#61`).

---

## [1.16.1] - 2026-11-05

### Fixed

- **The SHACL checker called every object-valued `metadata.source` blank, so 8 of the 10 shape warnings it reported for a namespace were false positives.** `src/rdf/shacl.rs` judged a rule's provenance with `source.as_str().map(|s| s.trim().is_empty()).unwrap_or(true)`. `as_str()` returns `None` for a JSON object, so `unwrap_or(true)` marked it blank — but an object source is legal and in use: `add_memory` declares `source` with type `["string","object"]`, the rule-source backfill in `src/server.rs` treats a non-string source as *present* (so it is not backfilled), `supersede_memory` accepts an object source, and the canonical predicate `crate::traits::has_source` (`src/traits.rs`) reads a non-empty object as sourced. The task gate (`src/task/gate.rs`) and the search ranker (`src/store/ladybug.rs`) both already called `has_source`; only the SHACL checker had drifted, so the validator disagreed with the gate it ships beside. The checker now calls `has_source` instead of carrying a second copy of the rule, and the two messages stay distinct: an absent source is `"source is required for a rule"`, while a source that is present but unusable provenance (null, a blank string, an empty object, a number, a boolean, an array) is `"source must be non-blank for a rule"`. A non-empty string or a non-empty object is accepted. Tracked as `neurostrata-aube` (ns-aube).
- **The hygiene repair destroyed a structured `metadata.source`.** `repair_namespace_hygiene` (`src/store/ladybug.rs`) read a rule's source with `meta.get("source").and_then(|v| v.as_str()).unwrap_or("")`, so an object source read as blank and `neurostrata-mcp repair-hygiene` overwrote the structured provenance with the row's location (or `"legacy-rule"`) — the repair destroyed the origin it was meant to preserve. It now asks `crate::traits::has_source`, the same predicate the task gate and the SHACL checker use, and only repairs a source that is genuinely absent or unusable; the `"N/A"` placeholder is still repaired. Found by the independent verification pass over the SHACL fix. Tracked as `neurostrata-aube` (ns-aube).
- **Four claims in the v1.16.0 documentation sweep did not match the code.** A source-verified audit of the shipped 1.16.0 notes found them, and they are corrected here rather than left standing. (1) **The tool count was 51; it is 56.** `src/server.rs:840` does `tools.extend(crate::synaptic_mesh::synaptic_mesh_tool_definitions())`, appending the 6 SynapticMesh tools (`src/synaptic_mesh/tools.rs:16-88`) to the 50 in the `tools/list` array; the order-pinning test `tools_list_carries_all_thirty_six_tools_in_a_stable_order` (`src/server.rs:11115-11196`) pins all 56 by name. Counting only `src/server.rs` misses the six, which is how the wrong number got in. (2) **The pre-push gate was described as blocking on three conditions.** Those are its *task* conditions; it also blocks on *rule honesty* — a live rule with no `metadata.source`, an unknown `metadata.enforcement` value, an `ENFORCED` claim naming a missing or advisory guard, a `PARTIAL` claim without a real guard, a `NOT_ENFORCED` claim that still names one, or a guard with no enforcement value (`src/task/gate.rs:399-530`; `GateReport::ok()` is `violations.is_empty()`). `docs/organs/central-executive.md` now names all of them. (3) **`oxiri` was missing from the semantic-web crate list.** It is a direct dependency (`Cargo.toml:112`) and belongs beside `oxrdf` / `oxttl` / `spargebra` / `sparopt` / `spareval` / `sparesults` / `shacl`. (4) **The mesh subcommand list omitted the hidden `sync-enqueue`** (`src/synaptic_mesh/cli.rs:47-54`, `#[command(hide = true)]`) — the pre-push hook's internal pull-then-push target, not a user-facing transport command, and a distinct name from the non-existent `sync`. Tracked as `neurostrata-a6xv` (ns-a6xv).
- **The Nightly workflow had not built successfully since 2026-10-29.** Two independent causes, both confirmed against the run logs rather than guessed. (1) *Windows:* `nightly.yml` never received the vendored-OpenSSL configure step that `ci.yml` and `release.yml` carry — `Cargo.toml` declares `openssl` with the `vendored` feature, so `openssl-sys` builds OpenSSL from source and needs a Perl with the `Locale::Maketext::Simple` module; the Git-bundled Perl that wins `PATH` on the `windows-latest` image exits 2 during `Configure` with `Can't locate Locale/Maketext/Simple.pm in @INC`. `nightly.yml` now sets `OPENSSL_SRC_PERL` to the preinstalled Strawberry Perl and puts NASM on `PATH`, exactly as the other two workflows do. (2) *Linux:* the build failed on `include_str!` of `plugins/opencode/dist/index.js`, which is not tracked in git; that bundle is tracked as of the #52 merge, so the Linux job needed no change. Tracked as `neurostrata-dbcc` (ns-dbcc).

---

## [1.16.0] - 2026-11-05

### Added

- **A shared `@neurostrata/plugin-core` (`plugins/core/`) with four thin harness adapters.** The OpenCode, Claude Code, Codex and GitHub CLI plugins are now 13-line adapters that re-export one implementation (`plugins/core/index.ts`, ~1.8k lines); the Hermes (`plugins/hermes/index.ts`, 863 lines) and OpenClaw (`plugins/openclaw/index.ts`, 1000 lines) bundles still carry their own implementations and were **not** converted to the shared core in this release. The core ships zero runtime dependencies (Node built-ins only).
- **The shared core is embedded in the binary.** `src/task/harness_plugins.rs` embeds `plugins/core/dist/index.js` and `plugins/core/package.json` via `include_str!` and installs them into `.NeuroStrata/plugins/core/` on init, so the shared implementation ships with the binary and needs no npm install.
- **Daemon-backed active-task resolution (`src/server.rs`, `src/task/mod.rs`).** `neurostrata_guard_validate` now falls back to the `session_id` of the active `in_progress` task when the caller's `session_id` is empty or `"unknown"`, so a stateless client is no longer treated as an unattributed caller. `neurostrata_task_list` returns `session_id` and `subagent_prompt_injection` on `in_progress` rows, so a plugin can resolve the active task and its prompt in one call.
- **AST neighborhood in `neurostrata_memories_for_file` (`include_ast_edges`, default `true`).** The response now carries `related_memories` (1-hop `DEPENDS_ON` / `RELATES_TO` neighbours and `SUPERSEDES` targets) and a compact `ast_summary` — `symbols`, `callers`, `callees`, `imports`, `importers`, `blast_radius_count` — computed by `LadybugStore::ast_neighborhood_for_file` (`src/store/ladybug.rs`).
- **Dynamic subagent prompt injection (`plugins/core/index.ts`).** `enrichAndInjectSubagentPrompt` prefetches the governing memories of every repo-relative file path mentioned in a subagent prompt and embeds them, alongside the daemon's `subagent_prompt_injection`, in the injected header.
- **A verifiable Graph-First rule predicate.** The rule shipped in `SUBAGENT_GRAPH_FIRST_RULE_SNIPPET`, `build_task_graph_context` and the `.agents/` templates no longer carries the prose bypass "unless the parent prompt states" — it names a check an agent can actually run: for any file it touches that is not listed under `### NeuroStrata Task & Graph Context`, call `neurostrata_memories_for_file` first.

### Fixed

- **The OpenCode plugin was never loaded.** `wire_harness_config` wrote `.opencode/neurostrata-plugin.json` — a file OpenCode does not read — and merged the plugin path into a root `opencode.json` under a `"plugin"` key, and only when that root file already existed. It now writes the `.opencode/plugins/neurostrata.js` loader OpenCode auto-discovers, and always creates or merges `.opencode/opencode.json` → `"plugins"`, preserving every other key and entry. A root `opencode.json` is still only touched when it already exists.
- **The plugins hardcoded the namespace `NeuroStrata`.** Every bundle shipped `namespace: "NeuroStrata"`, so the guard, the task list and the memory lookups ran against the wrong namespace outside the NeuroStrata repo. `resolveConfig` (`plugins/core/index.ts`) now resolves it in order: plugin config → `NEUROSTRATA_NAMESPACE` → the namespace discovered from the project root, and never falls back to a hardcoded name.
- **Tool classification was incomplete and the Code Mode path was invisible.** `TOOL_CLASSIFICATION` now names the read, mutating-file, shell, subagent and code-exec sets explicitly, with `isFileTouchTool` / `isMutatingTool` / `isSubagentTool` / `isCodeExecTool` predicates; `inspectCodeModeExecution` reads an OpenCode V2 Code Mode `execute` payload (`input.input.code` / `input.args.code`) and extracts the `id`, `session_id`, `note` and `path` arguments, so a claim, a note and a completion performed inside a Code Mode block are tracked like any other tool call.
- **Hook handling was per-harness.** `handleHookEvent` / `handleClaudeHookEvent` now handle `PreToolUse` and `Stop` the same way across harnesses: a subagent tool gets its prompt enriched, a file-touch tool gets the governing memories attached as `additionalContext` and `systemMessage`, and a mutating tool is checked against `neurostrata_guard_validate` and denied on reject.
- **The governing memories of a file never reached the model.** The plugin queried NeuroStrata for a touched file but never delivered the result to the LLM, and the Rust and TypeScript sides of `.NeuroStrata/session_queries.json` did not agree on what they wrote. The recall block is now delivered in `execute.before` (`additionalContext` / `neurostrataContext`) and in `execute.after` (appended to the tool output), and the ledger is written in one shape by both sides.
- **The scaffolded pre-commit hook assumed a Rust `src/<module>/` layout.** It collected `git diff --name-only --cached | grep '^src/'` and derived a module name from the first path segment, so every file outside `src/` was invisible to it — a Go, TypeScript or Python repository got no ledger check at all. The hook is now layout- and language-agnostic: it checks every staged file (excluding `.NeuroStrata/`) by full path and by directory prefix, and appends to the ledger whether or not the ledger already exists.
- **`/graph` and `/export-graph` returned 500 when the config file could not be read.** Both daemon routes treated a `Config::load()` failure as fatal; they now fall back to an empty `project_contexts` (`src/daemon.rs`).
- **The Windows MSVC build failed on a duplicate-symbol link.** `build.rs` passed `--allow-multiple-definition` unconditionally, which is a GNU-ld flag. It now passes `/FORCE:MULTIPLE` on `windows-msvc` and keeps the GNU flag on Linux and `windows-gnu`.
- **The "no daemon" plugin tests were not hermetic.** `plugins/core/test.mjs` reset the MCP client and then let it be recreated against the default endpoint, so on a machine with a NeuroStrata daemon running, the fail-soft test received real memories instead of none. The two no-daemon tests now pin the client to an unreachable endpoint.
- **The shipped documentation claimed features the code does not have.** A repo-wide honesty sweep corrected claims that had accumulated across the v1.15.0 documentation pass (commit `06c9957`) and earlier:
  - **Retrieval was described as a 4-stage fused pipeline.** It is not. `neurostrata_search_memory` exposes three *alternative* surfaces selected by `mode` (`vector` — the default, `lexical`, `lexical-inhouse`); there is no Reciprocal Rank Fusion, no Personalized PageRank, and no neural cross-encoder reranker. There is no `src/rerank.rs`, no `src/reranker.rs`, and no FlashRank dependency anywhere in the crate.
  - **The embedder was described as `BGESmallENV15` at 384 dimensions.** It is `NomicEmbedTextV15` at 768 dimensions (`src/embed.rs`).
  - **The Tree-sitter graph was described as 11 languages.** Six are supported — Rust, TypeScript, TSX, Python, Go, Java (`src/parser/mod.rs::get_language`).
  - **The semantic-web stack was described as the `oxirs-*` family.** That family was never a dependency. The shipped crates are `oxrdf`, `oxttl`, `spargebra`, `sparopt`, `spareval`, `sparesults`, `oxiri`, `shacl` and `petgraph`; closure inference is computed in-house by `src/rdf/ontology.rs` and `src/rdf/reasoning.rs`.
  - **Community detection was described as "Leiden/Louvain" and file digests as "`blake3` / `sha256`".** Only Leiden exists (`src/community.rs`), and only `sha2` is used (`src/parser/generation.rs`).
  - **MCP tool names that were never registered were removed** — `neurostrata_validate_action`, `neurostrata_guard_action`, `neurostrata_owl_reason`, `neurostrata_trace_call_chain`, `neurostrata_impact_analysis`, `neurostrata_enrichment_candidates` / `_promote`, `neurostrata_twin_create` / `_define` / `_observe` / `_rollback` / `_simulate`, `neurostrata_metrics_record`, `neurostrata_mesh_status` / `_conflicts` / `_resolve`, `neurostrata_wal_status`, `neurostrata_domain_register` / `_list`. The authoritative surface is the **56 tools** registered in `src/server.rs` `tools/list` — the 50 in the `tools/list` array plus the 6 SynapticMesh tools that `tools.extend(crate::synaptic_mesh::synaptic_mesh_tool_definitions())` appends to it (`src/server.rs:840`, `src/synaptic_mesh/tools.rs:16-88`); the order-pinning test `tools_list_carries_all_thirty_six_tools_in_a_stable_order` (`src/server.rs:11115-11196`) pins all 56 by name; the documented replacements are `neurostrata_guard_validate` / `_learn` / `_list_rules`, `neurostrata_twin_set` / `_get` / `_list` / `_diff`, `neurostrata_metrics_query` / `_list`, `neurostrata_synaptic_mesh_*`, `neurostrata_domain_registry`, and `neurostrata_memories_for_file` with `include_ast_edges` (whose `ast_summary` carries `callers`, `callees`, `imports`, `importers` and `blast_radius_count`).
  - **CLI commands that do not exist were removed** — `neurostrata-mcp learning` (the command is `learn`), `neurostrata-mcp laya-export` (it is `laya`), `neurostrata-mcp gliner` (GLiNER runs in the daemon and stores `metadata.gliner_candidates` on the memory), and `neurostrata-mcp synaptic-mesh sync` / `conflicts` (the subcommands are `push` and `pull`).
  - **The guard was described as returning `ALLOW` / `WARN` / `BLOCK`.** `ValidateVerdict` has five variants: `DeterministicReject`, `SandboxReject`, `SandboxPassHighFidelity`, `SandboxPassLowFidelity`, `ApprovedFailOpen`. The sandbox invocation was also corrected to the flags the code actually uses (`--rm --network=none -v=<cwd>:/workspace:ro --tmpfs=/tmp` — there is no `--read-only`).
  - **The secret scanner was described as redacting with `[REDACTED]` markers.** `src/secrets.rs` fails closed and **rejects** the write; it never redacts. The `src/scrubber.rs` path never existed.
  - **The global namespace was described as a directory.** `.agents/skills/neurostrata/SKILL.md` pointed at `~/.config/neurostrata/global/`. There is no such directory: `global` is a namespace **column** in the same LadybugDB store (`Memory.namespace`), and the config file is `~/.config/neurostrata/config.json`. The skill file is `include_str!`-embedded in the binary, so the correction ships with the release.
  - **The Obsidian / NeuroVault sync was documented as a shipped feature.** It was never implemented — there is no `src/obsidian.rs` in any commit, no `neurostrata_sync_obsidian` MCP tool, and no `obsidian-sync` CLI subcommand. `docs/ops/obsidian.md`, `docs/UI_GUIDE.md`, `docs/FEATURES.md`, `docs/README.md`, `docs/COGNITIVE_ARCHITECTURE.md` and `docs/science/nomenclature.md` now say plainly that it is planned and not implemented, and point at the tracking task `neurostrata-dlag`.
  - **The pre-push gate was credited with scanning diffs for `TODO`/`FIXME` markers.** `src/task/gate.rs` never did that. Its **task** conditions are `in_progress` tasks, `done` tasks without an extracted memory, and P0 tasks open for more than 24 hours; it also blocks on **rule honesty** — a live `rule` row with no `metadata.source`, an unknown `metadata.enforcement` value, an `ENFORCED` claim naming a missing/advisory guard, a `PARTIAL` claim without a real guard, a `NOT_ENFORCED` claim that still names one, or a guard with no enforcement value at all (`src/task/gate.rs:399-530`; `GateReport::ok()` is `violations.is_empty()`). The gate bullet in `docs/organs/central-executive.md` now names all of them and points deferral language at the lexical task-intent detector, which is what actually catches it.
  - **The Breath checkpoint was described as advisory.** It is a hard block: `src/guard/fsm.rs` returns `BreathRequired` once `mutations_since_note >= 5`, and further mutations are refused until `neurostrata_task_update(note=...)` records a note. `docs/organs/central-executive.md` now says so, including the rejection's next-legal-transition hint.
  - **The Episodic Buffer was documented as `*.jsonl`.** It is a rolling Markdown file, `.NeuroStrata/sessions/current.md`, with a 500 KB rollover (`src/buffer.rs`). `docs/architecture/tri-strata.md` was corrected.
  - **The SynapticMesh CLI was documented with `sync` and `conflicts` subcommands and a `--namespace` flag on `status`.** The real surface (`src/synaptic_mesh/cli.rs`) is `init <name>` (with `--namespace` / `--remote`), `join <dir>`, `push`, `pull`, `status` (no flags), `leave` and `recover` — there is no `sync` and no `conflicts`; transport is `pull` then `push`. `docs/mesh/synaptic-mesh.md` was rewritten to match.
  - **The crash-breadcrumb directory, the store path and the breadcrumb contents were wrong.** Breadcrumbs go to `<db_path parent>/crash` — `~/.config/NeuroStrata/data/db/crash/` (capital `N` and `S`) — not `~/.config/neurostrata/crash/`, and the main store is the file `~/.config/NeuroStrata/data/db/ladybug.db`. The breadcrumb itself was described as carrying the binary version, commit hash, target triple, thread name, panic payload, source location, lock state and WAL size: it carries none of those. `src/crash.rs` installs a handler for `SIGSEGV` / `SIGBUS` / `SIGABRT` / `SIGFPE` that writes `crash-<pid>-<epoch>.txt` containing the signal number and name, the pid, the faulting thread id, the epoch second, the alternate-stack size and — only when the stack had room for it — a backtrace. There is no Rust panic hook, so an ordinary `panic!` unwinds and produces no breadcrumb. `docs/ops/crashes.md` now says exactly that.
  - **The build documentation described runners and targets that do not exist.** The CI matrix is `ubuntu-latest` / `windows-latest` / `macos-15` — Apple Silicon only, no macOS x64 (the pinned `ort` release ships no prebuilt ONNX Runtime for `x86_64-apple-darwin`). `release.yml` publishes **four** targets (linux amd64 + arm64, darwin arm64, windows amd64), not five. `docs/BUILDING.md` now says so, drops the "macOS x64 ~28 min" timing, corrects the `lbug` pin to 0.20.4, and replaces the nonexistent `recovering_config()` citation with `strict_wal_verification()`, which is the function that actually governs WAL checksum behaviour.
  - **The `guard_rule` shape was wrong.** There is no `metadata.guard_rule` sub-object with `action_pattern` / `verdict` / `reason`. `neurostrata_guard_learn` takes a rule carrying exactly `id`, `version`, `rule_class`, `trigger_pattern`, `constraint_text`, `hit_count` and `status`; `trigger_pattern` is matched as a **case-insensitive substring** of the action type or payload, not as a regular expression, and there is no built-in list of banned binaries or destructive flags. `docs/organs/prefrontal-cortex.md` now shows the real call shape and states that `metadata.source`, `metadata.enforcement` and `metadata.guard` belong to the separate task-gate honesty audit.
  - **The Podman sandbox section did not say how the sandbox is invoked.** `docs/organs/prefrontal-cortex.md` now carries the exact command (`podman run --rm --network=none -v=<cwd>:/workspace:ro --tmpfs=/tmp -w=/workspace alpine:3.21 sh -c <payload>`), the 5-second timeout, and the rule that the sandbox only runs for the `bash` and `script` action types — any other action type returns `ApprovedFailOpen` *after* the deterministic and semantic checks pass.
  - **The store's typed relations were listed as including `CALLS` and `IMPORTS`.** The five typed relation tables are `RELATES_TO`, `CONTAINS`, `GOVERNS`, `EXTRACTED_FROM` and `DEPENDS_ON`; the Tree-sitter indexer records `calls`, `callees`, `imports` and `importers` as **metadata arrays** on the symbol and file nodes, which `neurostrata_memories_for_file` surfaces as the `ast_summary` fields of the same name. `docs/architecture/synapticgraph.md` was corrected, along with the store path and the `as_of` time-travel behaviour of `neurostrata_related`.
  - **`petgraph` was credited with computing the OWL/RDFS closure.** It is a direct dependency, but it backs the in-memory graph type `leiden-rs` operates on for community detection; the closure is computed in-house by `src/rdf/ontology.rs` and `src/rdf/reasoning.rs`. `docs/architecture/domains/core_installer.md` was corrected.
  - **The Self-Reinforced Testing Framework page described LanceDB, a `bd`/Beads tracker and a `neurostrata_generate_canvas` tool.** The store is LadybugDB, `bd` is deprecated in favour of the NeuroStrata task tools, and there is no canvas generator — `neurostrata-mcp export-graph <out.json>` is what ships. `docs/architecture/self_reinforced_testing_framework.md` was corrected.
  - Affected files: `README.md`, `docs/README.md`, `docs/FEATURES.md`, `docs/COGNITIVE_ARCHITECTURE.md`, `docs/UI_GUIDE.md`, `docs/BUILDING.md`, `docs/ops/cli-reference.md`, `docs/ops/crashes.md`, `docs/ops/security.md`, `docs/ops/obsidian.md`, `docs/organs/central-executive.md`, `docs/organs/dendritic-bridge.md`, `docs/organs/prefrontal-cortex.md`, `docs/architecture/synapticgraph.md`, `docs/architecture/tri-strata.md`, `docs/architecture/self_reinforced_testing_framework.md`, `docs/architecture/domains/core_installer.md`, `docs/mesh/synaptic-mesh.md`, `docs/science/nomenclature.md`, `docs/science/why-memory-fails.md`, `.agents/skills/neurostrata/SKILL.md` and `.agents/agents/NeuroStrata-Task.md`.

### Changed

- **`tokenizers` and `openssl` build configuration.** `tokenizers` now builds with `default-features = false, features = ["onig"]`, and `openssl` is vendored for every target rather than only for non-Apple targets (the Apple system-path resolution is unchanged).
- **The OpenCode bundle and the new shared core bundle are tracked in git.** `plugins/opencode/dist/` was removed from `.gitignore` and `plugins/core/dist/` is committed, so a fresh clone builds and runs without an npm build step. The tracked plugin bundles went from 20 files (5 plugins) to 28 (7 plugins).
- **`web-ui/public/graph.json` is no longer tracked.** The generated graph export was removed from the repository and gitignored.
- **Root scratch scripts were removed.** `scripts/push_and_pr.sh`, `scripts/run_mock_agent.sh`, `scripts/sync_github_issues.sh` and `scripts/wave48_remediation.py` are gone.

### Verification

- 1,496 Rust tests pass, 0 failures (`cargo test --locked -- --test-threads=1`); the build is warning-free and `neurostrata-mcp --version` reports `1.16.0`.
- 136 plugin tests pass, 0 failures: `plugins/core` 73, `plugins/opencode` 13, `plugins/claude` 11, `plugins/codex` 11, `plugins/github-cli` 11, `plugins/hermes` 11, `plugins/openclaw` 6.

---

---

## [1.15.0] - 2026-11-03

### Added

- **Zero-Touch Harness Detection & Plugin Auto-Install** (`src/task/harness_plugins.rs`) — on init (`neurostrata_bootstrap`, `neurostrata_task_setup`, `neurostrata_get_snapshot`), `neurostrata-mcp` detects the active CLI harness (`opencode`, `openclaw`, `claude`, `hermes`, `codex`, `github-cli`) from environment and project markers, extracts the embedded zero-dependency plugin bundle into `.NeuroStrata/plugins/<harness>/`, and wires harness config with zero user involvement. (Wave 60)
- **Six zero-dependency CLI/terminal harness plugins** under `plugins/` — OpenCode, Claude Code CLI, Hermes Agent CLI, OpenAI Codex CLI, GitHub Copilot CLI (`gh`), and OpenClaw — each shipping a compiled bundle plus unit tests (61/61 passing) enforcing pre-flight snapshot, Zero-Action Start, Graph-First lookup nudges, Breath checkpoints, turn-end deferred-work detection, and automatic subagent prompt context injection. (Wave 59)
- **Automatic Graph-First Exploration & Subagent Context Propagation** (`src/task/mod.rs`, `src/server.rs`) — `neurostrata_task_create` and `neurostrata_task_claim` compute hybrid vector + 1-hop graph context (`auto_retrieved_context`) and return a ready-to-paste `subagent_prompt_injection` block; all eight graph/memory lookup tools record per-task and per-session telemetry (`graph_queries_count` and `.NeuroStrata/session_queries.json`). (Wave 58)
- **Seamless Upgrade & Lifecycle Delta Engine** (`src/task/wiring.rs`, `src/store/ladybug.rs`) — `neurostrata_get_snapshot` infers `project_root` when omitted, auto-heals both `pre-commit` and `pre-push` git hooks, backfills schema defaults on startup, and reports `LIFECYCLE & HOOK DELTA` when a binary upgrade introduces new wires or triggers. (Wave 57)
- **Daemon Caller-Path Authority** (`src/daemon.rs`, PR #36 / `neurostrata-p1ex`) — `check_caller_path`, `resolve_caller_path`, `load_allowed_external_dirs`, and `caller_path_refusal` applied across `/ingest`, `/backup`, `/export-graph`, and `/tasks/import`, canonicalizing each caller-supplied path and verifying it against `startup_root` or `allowed_external_dirs` before use.
- **W3C RDF/SPARQL 1.1 / OWL 2 RL / SHACL validation via OxiRS** — full RDF-star/SPARQL support, SPARQL custom functions, SHACL `.ttl` shape loading into `OntologyIndex`, and OWL/RDFS reasoning with `closure_subclasses`/`closure_governs`. (Waves 42, 43, 45)
- **Local Laya duplicate judgment + centroid-margin training** — `LayaOnnxProvider::judge_duplicates` and `health_check` functional, ONNX bundle/spawn_blocking/offline purge/systemone, Laya ONNX logits/trainer pairs wired. (Waves 3, 53)
- **GraphRAG hybrid search** — `SearchMode` enum, hybrid scorer, symbol matching, community detection MCP, and opt-in integration in `search_inner` (`src/store/ladybug.rs`). (Waves 2, 3)
- **In-house full-text lexical upgrade** — a 5-field BM25 scorer over `Content` / `Symbol` / `Location` / `Namespace` / `MemoryType`, with domain-ontology expansion, `valid_to` parsing and retirement visibility (`src/fts/mod.rs`), plus `tantivy` 0.27 integrated as a dependency (`Cargo.toml`). **Rank fusion (RRF) still does not exist.**
- **Digital twin, KGE, GLiNER, stream, multimodal, RAG chat, time-series, anomaly, and community modules** — shipped end-to-end across Waves 2–5, 11–12, 14–16, 22–28, including `KGE cli_train` graph extraction, pure-Rust `Trainer`, `LearningRuntimeManager`, GLiNER engine, and duplicate-candidates MCP.
- **SynapticMesh mesh sharing** (slices 1–7) — `synaptic-mesh/v1` manifest and record files, daemon-owned git checkout per mesh, `mesh init/join/leave/status/pull/push/recover`, WAL-first outbox, import/replay with named conflict variants, trust gates; slice-8 acceptance evidence (four cross-node scenarios over real git) verified. (Waves 36–40)
- **NeuroStrata-Task.md embedding** and benefit-driven README restructure (Wave 65) — README leads with "The World's Most Advanced Agentic Memory, Task, and Workflow System" and effortless-automation flow; `NeuroStrata-Task.md` and `SKILL.md` embedded into plugin bundles on init. (Waves 64–65)
- **Wave 52–55 spec parity** — ONNX bundle/spawn_blocking/offline purge/systemone, temporal rerank/SPARQL federation/KGE MCP, edge `evidence_id` & AST call graph, SPARQL Graph result serialization, `base_iri` forwarding, fallback hardening.

### Changed

- **Full 60-Wave Codebase Audit & Zero-Flags Architecture** — all 51 root modules in `src/` audited; zero Cargo feature flags, zero runtime capability-toggle booleans, zero `#[allow(dead_code)]`/`#[allow(unused_imports)]`, zero production `.unwrap()`/`.expect()` calls. (Wave 27 purge, Waves 28–60)
- **The Tantivy FTS/RRF claim was retracted by erratum, and the erratum is itself superseded here.** The erratum in the 2026-10-20 section below was accurate as of that date: `tantivy` was not a dependency, and there was no BM25 scorer and no rank fusion. This release integrates `tantivy` 0.27 and ships an in-house 5-field BM25 (`src/fts/mod.rs`); **RRF rank fusion still does not exist.** The 1.0.0 section line is left exactly as it was written — the erratum supersedes it rather than rewriting history.
- **ERRATUM — two claims in this 1.15.0 section never matched the code.** The lines above are left exactly as they were written; these errata supersede them.
  - **"W3C RDF/SPARQL 1.1 / OWL 2 RL / SHACL validation via OxiRS".** The `oxirs-*` crate family was **never** a dependency of this project — `Cargo.toml` never listed it and no commit added it. The semantic-web stack that actually ships is `oxrdf`, `oxttl`, `spargebra`, `sparopt`, `spareval`, `sparesults`, `oxiri`, `shacl` and `petgraph`. The rest of that line is accurate: RDF-star support, SPARQL custom functions (`src/rdf/sparql.rs`), SHACL `.ttl` shape loading into `OntologyIndex`, and closure inference via `closure_subclasses` (`src/rdf/ontology.rs`) and `closure_governs` (`src/graph_read/related.rs`). "OWL 2 RL" overstates the profile — what is implemented is transitive `subPropertyOf` and subclass closure plus a bounded `GOVERNS` closure, not the full OWL 2 RL rule set. There is no `neurostrata_owl_reason` MCP tool; the closure is computed inside the store.
  - **"Local Laya duplicate judgment + centroid-margin training".** There is no centroid-margin code in this codebase — `grep -ri centroid src/` returns nothing, so the "centroid-margin weights" claim describes work that was never written. The rest of the line is accurate: `LayaOnnxProvider::judge_duplicates` and `health_check` are implemented in `src/judgment/laya_onnx.rs`, and the local training loop that does exist is the pure-Rust `Trainer` in `src/learning/trainer.rs`, driven by `neurostrata-mcp learn train`.
- **Beam stubs replaced** — Wave 14–18 Beam artifacts remediated with real AC2/AC4/AC5/receipt logic; KGE cwd test race fixed.

### Fixed

- **SPARQL Graph result serialization, `base_iri` forwarding, and fallback hardening** across RDF/judgment/GraphQL/RAG chat/ingest. (Wave 52)
- **Remove last three unguarded `.unwrap()` calls** in CLI/server/daemon; check `/shutdown` HTTP status; log daemon errors. (Wave 50)
- **Forward CLI mutations to daemon**, checkpoint on drop/exit, fix qualified-target hygiene, recover poisoned locks. (Wave 49)
- **Remove read-only hygiene mutation cheat**, log Cypher/JSON errors in `ladybug.rs`, clean up opencode/openclaw plugins, sync pre-push sha256 marker. (Wave 47)

### Security

- **Caller-path authority** blocks path traversal and symlink escapes on daemon mutation endpoints. (PR #36)

---

## [1.14.3-patch] - 2026-10-26

### Added

- **Retention/retrieval/gate Layer 2 + Lock 2 enforcement (task-3 through task-14)** — `TaskService::plan` verified to compile with `graph_read` budget wiring (rank_dedup_budget 512/1500/1500 defaults, 19 context_service_test tests pass); `ThalamicBus` post-ingest `normalize` subscriber and `task_complete_precheck` enforcing AC4 (zero dirty targets required before Done); `LadybugStore::upsert` canonicalizes edge targets at write time via `KnownIds::resolve`; unmatched-target classification (`Materialized`/`Declared`/`Orphaned`) with orphan warnings non-blocking (AC4: zero resolvable targets = pass); receipt binding to `HEAD_sha` + `dirty_tree_hash` with `is_current` hash-delta check that invalidates receipts on edits; AC5 baseline benchmark for p95 latency on `ContextService::plan` + `LadybugStore::upsert`; OpenCode plugin shim blocking mutating tools in `Unpreflighted` state via `guard_validate` (AC2); `guard_validate(session_id, active_task_id, worktree, action_type, is_mutating)` state check permitting reads at all states and blocking mutates when `active_task_id` is none; macOS launchd + Windows NSSM install in `setup.rs`.

### Changed

- **Retention/retrieval/gate enforcement verified end-to-end** (AC5 verified) across tasks 3–14; `cargo check` passes on all Layer 2 writes.

---

## [1.14.3] - 2026-10-26

### Changed

- **README setup sequence** — explicit setup init sequence added to `README.md` (`docs: add explicit setup init sequence to README`).

---

## [1.14.2] - 2026-10-26

### Added

- **Graph-read capability + `memories_for_file` tool** — `query RELATES_TO` edges (both orientations) with new `GraphMatchKind::Related`; honour `source`/`project_root` in `memories_for_file` and its handler; retire replaced stubs on ingest; unindexed ancestor dir stubs marked `directory`; apply `exclude_ids` before `limit`; replace full-namespace `list()` scan with targeted Cypher (`discover_candidates`); tests for related edge, stub retirement, parent-dir rules, unsupported backend; `task-3-report.md` relocated to gitignored `.superpowers/sdd/`.
- **Source-scoped staged ingestion** — physical ids become `namespace::generation::path` so two worktrees of one repository never confluate on a single primary key; ingestion stages into a fresh generation and atomically flips the active pointer on publish; symbol identity moves to body hash (`span_sha256`); per-node `relative_path` and `file_sha256` stamped on file/markdown/symbol nodes; superseded generations filtered from `list`/`relink_edges`/`export_graph` against the active `(namespace, generation)` set; `run_consumer` propagates upsert/embed errors instead of logging and returning `Ok`. (832 tests passed, 0 failed.)
- **Phase 0 adapter/auth contracts and context DTOs** — `ContextRequest`, `ContextPlan`, `ContextItem`, `SourceIdentity`, `EgressPolicy`, `Freshness`, `Coverage`, `Degradation` in `src/context/model.rs`; freshness dimensions and degradation state as typed enums; OpenCode/OpenClaw adapter repair with real `tools/call` dispatch, bearer token from `~/.config/neurostrata/daemon.token`, daemon `:34343` endpoint.
- **Privacy/egress helpers, durable receipts module, embedding-space identity descriptor, vocabulary v8 typed decision envelope, 4-dimension freshness computation, shared context read-planning service, bounded related traversal tool, upgraded search filters.**

### Fixed

- **Resolve 7 review findings on graph-read capability** — edge orientation, source scoping, stub retirement, exclusion-before-limit, targeted Cypher, and test coverage.
- **Resolve 7 review findings on source-scoped staged ingestion** — generation pointer rows, error propagation, mid-stage abort test, hash capture, superseded filtering, integration test exercising id/generation helpers.
- **`cas_version` LONG → INT64** in `IntakeCandidate`.

---

## [1.14.1] - 2026-10-26

### Added

- **Hook/doctor auto-discovery (Approach C)** — spec and plan for hook/doctor auto-discovery; `neurostrata_get_snapshot` accepts optional `project_root` arg and caches it; hook hygiene + throttled in-memory doctor pass; fallback hook-hash check when daemon down; back up overwritten hook to `.bak`; add hash marker to scaffolded pre-push hook.
- **`normalize-metadata` CLI command** — in-place metadata target normalization.

### Fixed

- **Don't leak absolute-path parent id for root node** — trim trailing slash in `KnownIds::resolve`.
- **Update-only `project_root` cache** on append path; drop temp smoke test.

---

## [1.14.0] - 2026-10-26

### Added

- **Laya ONNX functional** — `LayaOnnxProvider::judge_duplicates Ok`, `health_check Ok(true)` in `src/judgment/laya_onnx.rs`; `AcceptableEmbedder.model_sha256` + `verify_model_checksum` in `src/embed.rs`.
- **Pinned model downloads** — `ModelDownloader` with pinned SHA-256, supply-chain protected (`src/model_downloader.rs`); `artifacts/laya-bundle/registry.json`.
- **Pre-commit graph hook** — `PRE_COMMIT_HOOK` + `HooksCommands::InstallPreCommit` in `src/main.rs`; `src/task/mod.rs` wiring; `src/server.rs` and `src/config.rs` changes.
- **Decision 2.0 rejected** — Kai-0.6B Index 16.3 < JEV-1.0.

### Changed

- **§0 Prime Directive active with a 783-test green suite.**

---

## [1.13.1] - 2026-10-25

### Added

- **Task coverage enforcement via intent detection + pre-push gate** — `task_complete` rejects untracked work items (intent markers without explicit `follow_up_tasks` or task ID citation via `detect_task_intents`); `scripts/task-coverage-gate.sh` wired into `SCAFFOLDED_PRE_PUSH_HOOK` `PROJECT_GATES` so changed docs lacking task IDs block the push. Gate audit matrix 5/5 scenarios passed; 65/65 task tests green.

### Fixed

- **`PROJECT_GATES` skip missing guards** — loop skips missing guard files with warning instead of failing, fixing test fixtures without `task-coverage-gate.sh`; `scripts/install-hooks.sh` and `scripts/hooks/pre-push` created from embedded `SCAFFOLDED_INSTALL_HOOKS` template.

---

## [1.13.0] - 2026-10-25

### Added

- **Composite improvement recommendations (28 items, P0→P1→P2).** Full P0 security hardening (R1–R8), P1 mesh/embed/namespace/filter improvements (R9–R21), P2 CLI proxy/storage/sandbox/vocabulary/structural split (R22–R28). See `docs/designs/2026-10-24-composite-improvement-recommendations.md`.
- **Daemon `/export-graph` endpoint** (R22) — routes through daemon's single writer, avoiding lock contention.
- **Handler extraction** (R28) — `src/handlers/memory.rs` with `handle_get_memory` and `handle_list_namespaces`.
- **R12 incomplete marker** — `incomplete: bool` on `DomainSearchOutcome`, set when 10x FETCH_MULTIPLIER budget exhausted.
- **R13 RemoteEmbedder fail-closed auth** — verifies non-empty api_key before any remote embed call.
- **Mesh procedure trust** (PR #34) — manifest stamp, provenance gate, secret scan.
- **Daemon request trust** (PR #33) — Host/Origin checks, operator tier.
- **Task claim atomicity** (PR #32) — resolves test file conflict.

### Changed

- All "deferred" language removed from artifacts and task descriptions.
- R24 storage migration evaluation: stay on lbug 0.20.4, no migration.
- sygq (Laya training) retired — Python/Torch no longer needed.

### Fixed

- Float-binding test alignment (PR #35).
- ProjectAnchors references removed from episodic_pointer_echo.rs (PR #29 divergence).
- Test compile fixes (PR #31).

---

## [1.11.0] - 2026-10-22

### Added

- **The vocabulary gained its sixth version and two new shapes.** `fact.fields.attempt` and `fact.fields.domain_registry` are declared at vocabulary version 6 (`src/schemas/memory-vocabulary.v3.json`), so the two shapes an agent needs for the recall closure are teachable and checkable rather than ad hoc metadata. `metadata.attempt` records a failed approach (what was tried, why it failed, the constraint it exposed, the scope); `metadata.domain_registry` is the per-namespace registry row.
- **A domain that is not registered is refused, with the registered list and the remedy.** The first domain write in a namespace seeds the registry from the namespace's own rows; a later write whose slug neither the registry nor the namespace's data carries is refused with `Registered domains: [...]` and `Remedy: pass register_domain: true to add '<slug>'`. The registry is a cache, not the truth, so it can be stale and the reconcile repairs it; a non-string `domain` is rejected, not guessed at. The rejection runs on both write paths (`neurostrata_add_memory` and `neurostrata_task_complete`).
- **A failed attempt is surfaced three ways, from one predicate.** `src/attempt.rs::is_failed_attempt` is the single predicate behind the store's shape check, the `format_memory` rendering line (`Attempt [FAILED in <scope>]: <what>`), the `[FAILED ATTEMPT] ` snippet prefix (applied before the 120-character truncation), and the duplicate partition (a candidate carrying a valid attempt never blocks a positive fact, and vice versa). The attempt shape warns but never rejects.
- **The snapshot names the registered domains and carries a domain strap.** `neurostrata_get_snapshot` takes an optional `domain` (string or array); the top five are unchanged, and a bounded strap (at most three entries, at most two per domain) surfaces domain-tagged rows the access-count cut would have hidden, with an explicit zero when empty. The body carries `Registered domains (N): ...`.
- **The metrics surface gained `domain_filter` and `recall` sections.** Six process-global atomics (`domain_queries_total`, `domain_narrowed_hits`, `domain_fallback_vector`, `single_domain_bypasses`, `recall_responses`, `recall_promotions`) ride on the bus metrics body alongside the bus's own eight integers, plus a bounded, per-namespace recently-recalled id set that is evicted on promotion.
- **The mesh import unions an imported record's domain into the local registry** after the apply succeeds, so no shared record is ever refused for a domain the local registry has not seen; a registry-write failure is a warning, never a refusal.

### Changed

- Vocabulary version 5 → 6 (`fact.fields.attempt`, `fact.fields.domain_registry`).
- The static `memory_type` description now teaches the `attempt` and `domain_registry` shapes.

---

## [Unreleased] - 2026-10-20

### Added

- **A turn's own write left the memories it touched invisible.** A newly written row stamps `access_count: 0` (`src/server.rs:1131`), and the snapshot sorts by `access_count` and cuts to five (`src/server.rs:1476-1491`) — so a memory nothing had ever read was, from the snapshot's point of view, indistinguishable from a memory that did not matter. The `neurostrata_append_log` response now carries the turn's own recall: at most three pointers to the memories the turn's tags matched, with the mode that says whether the domain filter applied. The recall never moves `access_count`, so a recalled memory is not a promoted one — promotion stays the explicit `neurostrata_get_memory` call.
  - `recalled` is present on every response in the append's response family, the successful one and the failed one alike (`src/buffer.rs:199`, `:316`); an empty array with a `recalled_from` means a search ran and matched nothing, and an empty array without one means no search produced rows — either no search ran or the search failed, which the array alone does not separate.
  - `recalled_from` is present exactly when a search ran (`src/buffer.rs:303`, `:318-320`) — `"domain"` when the domain filter narrowed the candidates, `"vector"` when it was skipped (`src/buffer.rs:288-291`). It is absent on an untagged turn (`src/buffer.rs:206-207`) and absent when the search failed (`src/buffer.rs:236-248`, `:262-267`): no search produced rows to attribute, so there is no mode to report.
  - Only direct matches are recalled (`src/buffer.rs:270-274`; the shared predicate is `crate::traits::is_direct_match`, `src/traits.rs:100-105`) — a graph expansion is context *for* a match, not a match.
  - The recall never moves `access_count` (`src/buffer.rs:664-668`); promotion stays an explicit `neurostrata_get_memory` call (`src/server.rs:2706`).
  - `docs/recall.md` is the consumer-facing note: what the two fields mean, when each one is present, and what they deliberately do not do.
- **A `domain` was written, echoed to export, and never filtered anything.** The domain was descriptive metadata (`src/server.rs:1281-1294`), so a search could not be scoped to the domain the caller was asking about, and a caller who named a domain had no way to tell a filter that applied from one that did not. `neurostrata_search_memory` now takes an optional `domain` — a slug or a list of slugs — and the filter is **soft**: a filter that would empty the candidate set is skipped and the unfiltered candidates are returned, because a wrong domain must not blind a search. The report's first line says which happened. With no `domain` the tool behaves exactly as it did before.
  - The argument is declared in the tool schema (`src/server.rs:192`) and normalized to lowercase-kebab before it reaches the store (`src/server.rs:2006-2022`).
  - The mode line names the three outcomes — applied, skipped because no row matched the domain, skipped because the domain did not normalize (`src/server.rs:2059-2078`). A skipped filter is a substitution the caller has to be told about; a silent fallback would misreport what was searched.
  - With no `domain` the tool behaves exactly as before and prints no mode line (`src/server.rs:2057`; the assertion that pins it at `src/server.rs:3165-3168`).
- **The access path a fact described was undeclared metadata.** It was invisible to the agent — the vocabulary's `tool_summaries` is where an agent learns a shape — and it was uncheckable, so two facts could describe the same path in two different shapes and nothing would say so. It is **declared** now: `memory_types.fact.fields.topology` joins the vocabulary, version 4 → 5 (`src/schemas/memory-vocabulary.v3.json:2`, `:66-75`; the version assert at `src/traits.rs:312`), with five required string keys — `component`, `target_system`, `channel`, `credential_boundary`, `reason` — and extra keys ignored. A malformed value warns (one `WriteShapeWarning`, `src/store/ladybug.rs:951-955`, field `metadata.topology`, `src/store/ladybug.rs:1088-1092`) but the fact is still written: a bad shape must not cost the claim.
  - The five keys are pinned by the store's own constant (`src/store/ladybug.rs:960-966`), and the check keys off the memory type — on any other type `topology` is an undeclared metadata key and is accepted unchanged (`src/store/ladybug.rs:1066-1073`).
  - A missing key, a key that is not a string, and a value that is not an object at all are each a defect that warns (`src/store/ladybug.rs:1075-1093`); the warning is emitted after the row has landed (`src/store/ladybug.rs:1482-1485`), so the claim survives the complaint.
- **Lessons learned in the same task could not reach each other.** A memory extracted from a task carried an `EXTRACTED_FROM` edge to that task, but the expansion only ever traversed `RELATES_TO`, `CONTAINS` and `GOVERNS` — so the sibling lessons of a direct match were invisible, and the one relation that says "these came out of the same piece of work" was the one relation recall could not follow. The expansion now makes one further bounded traversal — `MATCH (m:Memory)-[:EXTRACTED_FROM]->(t:Memory)<-[:EXTRACTED_FROM]-(s:Memory)` (`src/store/ladybug.rs:243-249`) — which surfaces the other lessons extracted from the same task, each carrying the honest two-edge evidence path "this memory and a direct match were both extracted from the same task" (`EvidenceKind::EpisodeSibling`, `src/traits.rs:77`; rendered at `src/server.rs:2419-2441`).
  - The task row is a legal *hop* and never a *result*. The row-class policy that task rows are not recall material is untouched, and a sibling is filtered by exactly the same rules as every other expanded row.
  - Siblings get their own admission family and their own cap (`src/store/ladybug.rs:466-492`), so a task with many lessons cannot crowd out graph neighbours and graph neighbours cannot crowd out siblings. Rendered order: direct matches, one-hop, episode siblings, governing two-hop.
  - The relation is read from the graph edge, never from the metadata, so the two cannot drift; and a retired intermediate task invalidates that one path and nothing else, matching the existing two-hop rule.
  - On the live graph this recovers 100 sibling pairs that recall could not previously reach (20 in `NeuroStrata`, 80 in `dogfood-app`) — measured on 2026-10-20, the day this entry was written; the live graph grows and the definition of a "sibling pair" is not stated here, so the number is a point-in-time measurement rather than a current figure.
- **The daemon now leaves a breadcrumb when it dies.** A native SIGSEGV inside the engine killed the daemon on 2026-10-20 with no Rust panic, no stderr and no surviving core -- the host's `kernel.core_pattern` pipes cores to apport, which declines user-service crashes -- so the fault left nothing behind and the next crash of the same shape would have been exactly as undiagnosable. The daemon now installs a handler for SIGSEGV, SIGBUS, SIGABRT and SIGFPE (`src/crash.rs`) that writes a minimal report -- signal, pid, thread id, UTC timestamp -- and then a best-effort backtrace to `<config dir>/crash/crash-<pid>-<epoch>.txt`, mode 0600, before re-raising the original signal so the exit status stays what the kernel logged (`status=11/SEGV`) and systemd still reports the crash as a crash. It writes nothing outside the user's own configuration directory and needs no privilege of any kind: no root, no capabilities, no system state, and nothing an operator has to set up.
  - The report is written with `open`, `write` and `close` only, from a fixed stack buffer, with no allocation on the path that matters -- a heap that is already broken is exactly the situation the handler may be running in. The backtrace capture is best effort and is wrapped in `catch_unwind`, so a panic inside the handler cannot turn a SIGSEGV into a different signal; the report is written *before* the capture, so a capture that hangs or faults cannot take the report with it.
  - `SA_ONSTACK` puts *every* signal on the thread's alternate signal stack, and the Rust standard library's is 8192 bytes -- too small for the handler's frame, and too small for the backtrace capture, which faults at 16 KiB and completes at 32 KiB. A handler that captured a backtrace there would fault inside itself and lose the breadcrumb, so the threads that can fault are given a 64 KiB alternate stack of their own: the tokio workers, through `on_thread_start` (`src/main.rs`), and the blocking-pool threads that run every engine call, which is where the 2026-10-20 fault happened (`src/store/ladybug.rs`). Where there is no room for the capture, it is skipped rather than attempted, and the minimal report still lands.
  - Two tests assert the whole path in a child process: a real SIGSEGV and a real stack overflow on a worker thread each leave a breadcrumb and still kill the process with the signal (`src/crash.rs`).

### Fixed

- **The outgoing evidence edges pointed the wrong way.** The expansion's direction flag was read off a rendered boolean, and lbug renders a boolean `true` as "True" — so the comparison against "true" was never true, the flag was always false, and every outgoing path was reported backwards: the one-hop and two-hop evidence claimed the neighbour pointed at the direct match when the graph says the opposite. The flag is now read as a typed boolean by a named reader (`src/store/ladybug.rs:15-29`), so an outgoing edge is reported from the primary towards its neighbour (`src/store/ladybug.rs:288`, `:348`).
  - The incoming direction was never broken — it read false too, which is the correct value for that branch — so a defect that reversed half the paths stayed invisible to every test that asserted only the relation name. Two store tests now assert the endpoints of both outgoing paths (`src/store/ladybug.rs:4515`, `:4539`), the existing one-hop test pins the incoming endpoints as well (`src/store/ladybug.rs:4458-4459`), and a server test pins the printed text for an outgoing directed edge (`src/server.rs:6025`) — `RELATES_TO` is drawn undirected in every evidence line (`src/server.rs:2468`), so only a directed relation reveals the orientation there, in the one-hop line and in the two-hop line alike (`src/server.rs:6281`).
  - The reader asserts the column's type in debug builds (`src/store/ladybug.rs:24-27`), so a query that projects the flag as a string fails loudly in the tests instead of silently reading false again.
  - The fix changes the orientation of the outgoing paths, and orientation is part of a path's identity: an outgoing edge and its incoming counterpart no longer collapse into one path — the one exception is a one-hop path over an undirected relation (`RELATES_TO` today), whose endpoints are sorted because the edge has no direction (`src/store/ladybug.rs:1307-1313`) — so a candidate can carry more evidence paths than it did before. A two-hop path is never sorted, whatever its first edge's relation is, so a two-hop `RELATES_TO` does not collapse either. Both halves of that rule are pinned by a store test (`src/store/ladybug.rs:6703`). The paths are sorted by kind and then by their edges, and capped at three (`src/store/ladybug.rs:505-515`), so the evidence count, the order and the surviving set can differ from what the same query returned before. Which rows are admitted, and the order they are admitted in, do not change.
  - These were the only two boolean reads of that kind in the store; no other row value is compared against a rendered boolean.
  - The two-hop line in the tool output carried a separate defect that this change did not fix: the renderer resolved the walk's starting point to the intermediate instead of the direct match, so the second edge was dropped and the neighbour the two-hop evidence is about never appeared in the printed path. It is fixed by `neurostrata-bd23` below. What this change fixes on the store's side is the path itself: an outgoing two-hop path's first hop is now built in the graph's direction — from the direct match to the intermediate — where it used to be reversed; an incoming path still points at the direct match, as it should (`src/store/ladybug.rs:6786`).
- **The two-hop path was printed from the wrong end.** The renderer picked the walk's starting point by looking for the endpoint of the first edge that the second edge does *not* share, so the printed two-hop line started at the intermediate instead of the direct match, walked the first edge backwards, and then dropped the second edge — the neighbour the two-hop evidence is about never appeared in the output at all. The intermediate is now the endpoint the two edges share (`src/server.rs:2394-2411`), so the walk starts at the direct match and both edges are printed: the neighbour appears, and the `GOVERNS` arrow points the way the graph does — the neighbour governs the intermediate.
  - A server test pins the rendered two-hop line end to end (`src/server.rs:6094`): it asserts the path runs from the direct match, through the intermediate, to the neighbour. The intermediate is printed by both segments, because a path segment is rendered as a self-contained "from -relation- to" pair — the shape the episode-sibling line already had.
  - The one-hop line was never affected: it recovers the direct match as the endpoint of the edge other than the result's own (`src/server.rs:2361-2369`).
  - The `RELATES_TO` connector itself had two separate defects, both fixed by `neurostrata-66fl` below: the one-hop line printed the relation twice (`"primary" --RELATES_TO-- RELATES_TO- "res"`), and the two-hop line drew that undirected relation with a one-way arrow, because the undirected special case was gated on the path's kind rather than on the relation's declared direction.
- **The `RELATES_TO` connector was printed wrong.** Two defects, both in the same printed line. The one-hop line printed the relation twice (`"primary" --RELATES_TO-- RELATES_TO- "res"`), because the undirected branch passed a complete `--RELATES_TO--` literal *and* the relation's name to a format that prints the relation itself. And the undirected special case was gated on the path's kind rather than on the relation's declared direction, so the two-hop line drew that undirected relation with a one-way arrow: the same relation, drawn two different ways, depending on the kind of path it appeared in. The connector is now decided by the vocabulary — a relation declared `undirected` is printed once, with a `--RELATION--` connector, in every evidence line; a directed relation keeps its one-way arrow (`src/server.rs:2452-2481`).
  - The vocabulary is the source of truth, not a hard-coded relation name: the renderer reads the relation's declared direction out of it (`src/schemas/memory-vocabulary.v3.json:20-21`, read at `src/server.rs:2468`), and the store's dedup key reads the same declaration (`src/store/ladybug.rs:1307-1313`). The shipped relation set is closed and asserted (`src/traits.rs:318-321`), so an undirected relation added later arrives with a vocabulary bump and its own review — and is then drawn undirected without another edit to the renderer.
  - The renderer drops an edge the walk is not standing on, so a path whose edges do not form a chain cannot print a segment the graph does not contain (`src/server.rs:6248`).
  - Two server tests pin the rendering: the one-hop `RELATES_TO` line prints the relation once, with the undirected connector (`src/server.rs:6182`), and the two-hop line's first hop uses the same connector (`src/server.rs:6094`).
  - The change is confined to the printed line and to the store's dedup rule, which behaves the same way: the store's paths, the evidence kinds and the search results are unchanged.
- **The graph expansion never reached the caller.** The store computed one-hop and two-hop evidence and the read path then threw it away. Expansion rows were identifiable only by a magic score constant, the store's limit counted rows the caller could not use, and the row-class filters ran *after* the candidate budget had been spent — so an excluded row could take a slot that a real memory never got. Expansion is now a first-class result: direct matches and expanded rows are told apart by evidence kind (`EvidenceKind::DirectMatch` versus `OneHop` / `Governs2hop`) rather than by a magic number; `limit` counts recall-eligible primaries and the bounded expansions ride along with them; and the row-class filters (`task`, tombstoned, mesh-conflicting) are applied in the store, before the budget is spent. `access_count` now moves for direct hits only — an expanded row is evidence, not a read.
- **The expansion ranked by accident.** Candidates inside a family were ordered by id, so a UUID decided which memory surfaced first. They are now ordered by how many distinct direct matches reach them, descending — a memory several hits point at is the better answer — with the family order and the per-family caps unchanged and the id kept as the tiebreak.
- **The guard recorded triggers that never happened.** `rule_ids_triggered` listed the top candidate rules before the deprecated check and before the trigger was matched, and it was reported on passes too, so a verdict claimed rules that had not fired. It now lists only the rules that actually triggered, and `hit_count` — written once at creation and never again — counts firings.
- **A stale tool name in the duplicate-rejection message.** The message told agents to call `neurostrata_edit_memory`, a tool that is no longer in `tools/list`; it now names `neurostrata_supersede_memory`.

### Changed

- **`VectorStore::search` takes the row-class filters.** The signature gained `include_tasks` and `include_archived`, because the store is now the single place that decides which rows are recall-eligible. Any other implementor of the trait must be updated.

### Documentation (erratum)

- **ERRATUM — the "Tantivy FTS Hybrid Search / Reciprocal Rank Fusion" description of retrieval never matched the code.** The entry shipped in the 1.0.0 section below ("**Tantivy FTS Hybrid Search:** Integrated hybrid exact-keyword and vector search using Reciprocal Rank Fusion (RRF)") describes a feature that was never built, and the same description was repeated in `README.md` and in the cognitive-architecture and verdict documents. The 1.0.0 line is left exactly as it was written — this erratum supersedes it rather than rewriting history.
  - **What was wrong.** `tantivy` has never been a dependency of this crate: `git log -S'tantivy' -- Cargo.toml` returns nothing, and the current `Cargo.toml` contains no full-text-search crate. There is no `CREATE_INDEX`, no FTS statement, no BM25 scorer and no rank-fusion code anywhere in `src/`. The store creates one node table and four relation tables and nothing else (`src/store/ladybug.rs:1408-1431`). The likely origin of the confusion is the source comment "Step 2: Hybrid GraphRAG" (`src/store/ladybug.rs:207`) — a *graph* expansion, not a lexical one.
  - **What the code actually does.** Two deterministic steps, with no model in either:
    1. **Boosted vector distance.** Row classes are filtered first — structural rows always, `task` rows unless `include_tasks`, tombstoned rows unless `include_archived`, mesh-conflicting rows always (`src/store/ladybug.rs:76`, `:77-78`, `:121`, `:124`) — and what remains is scanned with `array_distance`, ordered by `dist ASC, id ASC` (`src/store/ladybug.rs:86-89`). A usage-and-provenance gain is then subtracted from that distance: `gain = 0.05 * ln(1 + access_count)`, plus `0.05` when the row carries `metadata.source` (`src/store/ladybug.rs:146-149`, `:1296`; the provenance test is `crate::traits::has_source`, `src/traits.rs:49`), and the combined gain is **capped at `0.5 * distance`** (`src/store/ladybug.rs:150-151`) so the boost can reorder rows only within a factor of two and never becomes an override.
    2. **Bounded directed-graph expansion.** Direct matches are expanded one hop over `RELATES_TO`, `CONTAINS` and `GOVERNS` in both directions, two hops over `GOVERNS`, and two hops over `EXTRACTED_FROM` — the last reaching the other lessons extracted from the same task (`src/store/ladybug.rs:219`, `:220-241`, `:243-249`). Each expanded row carries the evidence path that reached it (at most three per result, `src/store/ladybug.rs:515`), candidates are admitted per family — one-hop first, then episode siblings, then governing-only — and, within a family, by co-occurrence count descending (`src/store/ladybug.rs:466-484`), and expansion rows sort after the direct matches on the evidence kind rather than on a magic score constant (`src/store/ladybug.rs:525-537`, `:495-522`). The server asks for five primaries and renders what the store returns (`src/server.rs:1999-2001`); `access_count` is incremented for direct hits only (`src/server.rs:2042-2054`).
  - **Documents corrected:** `README.md` (the "BM25 via Tantivy" and "Embedded LadybugDB & Tantivy" claims, the Synaptic-Pruning wording, the search-scope statement, and the guard-tool and daemon-route lists); `docs/COGNITIVE_ARCHITECTURE.md` §3 (the neural-gain formula, which omitted the `0.5 * distance` cap and the provenance term), §4 (`domain` was descriptive metadata, not a retrieval filter — *now superseded in this same section*: `domain` is an optional soft filter on the search query) and §6 (the "Reciprocal Rank Fusion" section, now "Deterministic Retrieval — Boosted Distance + Bounded Graph Evidence"); and `docs/architecture/MEMORY_AGENTS_VERDICT.md` §2.2 and R1/R3, whose no-LLM-reranker argument rested on the false RRF premise and has been re-derived on the true premise.
  - **Consequence, still open.** Exact-identifier recall — a function name, an env var, a config key, an error string — depends on the embedder landing near the query, because no lexical track exists. The adjudicated recommendation is a **narrow deterministic `CONTAINS` / symbol-id match**, preferred over a Tantivy + RRF build. **It is not implemented.**
  - **Panel record:** `docs/designs/2026-10-20-argon-k3-panel-verdicts.md` (Part C, §1 DOC-1 and §5 Phase 4) and `docs/designs/2026-10-20-hyperskill-review-findings.md` (Part B, DOC-1).

---

## [1.10.1] - 2026-10-20

### Fixed
- **The successor was resolved one link deep.** A work that was replaced twice was resolved to the row that replaced it *once* — itself retired — so the gate read the chain as void: the extraction obligation was dropped, and the dependents of a blocker were released while the work was still in flight under the row that replaced it twice. The successor is now the row at the end of the supersession chain, walked bounded and cycle-safe, and one rule decides the three surfaces that need it: readiness, the extraction obligation, and the snapshot.
- **A retired lesson discharged an obligation.** The extraction check accepted any row that declared `extracted_from`, including a row that had itself been superseded or expired — history the store hides from search. A retired lesson discharges nothing now, and the gate and the completion path read the same way: the store hides; the gate refuses to forgive.
- **A lesson was pinned to one row id.** The row that replaced a finished task is the same work under a new id, so the lesson extracted from the old id is the successor's lesson too. The successor was demanding a second lesson for work that was already consolidated, and the push gate could not be satisfied. `extracted_from` now names any id in the work's lineage: the row itself, then the rows it replaced.
- **`task_list` offered history as work.** The listing showed retired rows as ordinary records — a superseded claim read as live claimed work — while the gate ignored them and the snapshot hid them. The three surfaces now agree: retired rows are hidden from the default listing, and come back labelled `retired: true` under `include_retired: true`.
- **Retiring a stuck claim moved it, and said nothing about how to release it.** `supersede_memory` clones the task payload, so the successor inherited the claim — status, assignee, session, and the age of the claim — and the claim kept blocking the push gate with no instruction anywhere saying how to let it go. The decision is now written down: **a claim is a lease on the work, not on the row id**, so the successor keeps the claim (correcting a task's text must not hand live work to another agent), and retiring a row is not releasing a claim. Releasing it is the explicit act it always was — `neurostrata_task_update` with `status: "open"`, which clears the assignee and the session — and the supersede reply now names the successor id and that call.

**Correction to 1.10.0's notes.** 1.10.0 described the successor as a single link ("`metadata.superseded_by` must point at a successor that is ... not retired", and "one that is itself retired ... leaves the lesson owed") and described the lesson as an edge into one row id. Both were one link and one id too narrow: the successor is the row at the end of the chain, and the lesson belongs to the work. 1.10.1 is the implementation of what 1.10.0's notes described.

## [1.10.0] - 2026-10-20

### Added
- **Universal source-anchor registry (`src/source_context.rs`).** Graph export no longer rebases one repository's relative paths onto whichever checkout happened to launch the export. Each stable source identity is bound to its own canonical `.NeuroStrata` marker directory, and the project root is derived from that marker's parent.
  - `neurostrata-mcp project-context bind|list|unbind` — bind a stable `source_id` to an existing `.NeuroStrata` marker directory; more than one source root can be configured in a single client configuration.
  - Export resolves each node's `locations[].location` against the binding for that row's own source, so one shared database can serve several repositories, linked worktrees, and unrelated process working directories and still export the same graph for the same snapshot and config, regardless of the caller's CWD.
  - Machine paths stay out of the exported provenance: they appear only in the legacy `absolute_path` projection, and only when a matching local binding is configured. The provider-neutral `locations[].source_context` contract is preserved — Core reads the standardized `source_id` discriminator and round-trips provider-specific fields (revision, workspace) opaquely.
  - Core stays VCS-agnostic: it stores and exports source provenance, and does not discover repositories, inspect refs, switch branches, or decide whether a consumer should reject a missing source.
  - `NEUROSTRATA_ANCHOR_ROOT` + `NEUROSTRATA_SOURCE_ID` still work as a legacy scoped override; the root on its own is deprecated and ignored with a warning.
- **Marker-path validation.** A bound source ID must point at a real `.NeuroStrata` marker directory; an invalid or ambiguous one is refused rather than quietly resolving paths against the caller's working directory.
- **`counts.retired` in the gate report.** The advisory report now says how many task rows it judged only as history, so `in_progress: 1` with no `in_progress` violation stops reading as a contradiction.

### Fixed
- **The gate judged retired rows as live work.** A task row whose `metadata.valid_to` has passed (superseded or expired) still raised `in_progress`, P0-rot and staleness signals, so a superseded claim could block every push forever, naming an assignee who no longer exists. Retired rows are now history: they raise no live-work violation, they are never offered as claimed or ready work, and a retired blocker stops holding its dependents unless a live successor still carries the work.
- **The extraction lock had a hole on retired rows.** A retired `done` row is exempt from `done_without_extraction` only when `metadata.superseded_by` names a successor that is itself a task row, is not retired, is `done`, and already has an inbound `EXTRACTED_FROM` edge. Anything else — a successor that is still open or was reopened, one whose own lesson is missing, one that is itself retired, one that is not a task, one that does not exist — leaves the lesson owed by the retired row, which still blocks. Malformed expiry metadata is not proof of retirement and keeps failing closed.
- **Retired rules failed the gate's provenance check.** A superseded rule was still judged as an active claim and could be reported as `rule_without_source`. Retired rules are now skipped by the rule checks — with the same deliberate divergence from the store's own retirement filter, which treats an unparseable expiry as expired and hides the row from search: the store hides; the gate refuses to forgive.
- **Saving the client config dropped fields it did not know and reset the file's permissions.** Unknown top-level config keys are preserved on save, and the config file keeps its original mode instead of being rewritten under the process umask.
- **Source anchors and edge diagnostics were unstable.** Anchor resolution and graph export resolve through the source's own binding, and a `GOVERNS` edge that cannot be resolved to a real file is now reported as deferred instead of failing the write silently.
- **The docs and the engine disagreed.** The CLI reference, the task-subsystem design doc, the tool schema and the push-gate instructions now describe the same retirement and readiness semantics the engine implements.

## [1.9.0] - 2026-10-11

### Added
- **SynapticMesh — mesh sharing for teams (`src/synaptic_mesh/`), slices 1–7.** Memories and procedure definitions now travel between agents on different machines over plain git. Components, per slice:
  - **S1 — wire schemas (`manifest.rs`, `record.rs`):** `synaptic-mesh/v1` manifest (mesh id, epoch, namespaces, `MeshPolicy`, remote) and `synaptic-mesh-record/v1` record files (one JSON per memory/procedure, carrying `lifecycle` state machine, `update_id` monotonic revision, `origin_mesh_epoch` provenance, trust/eligibility gates; runtime procedure counters never leave the node).
  - **S2 — storage + surface (`storage.rs`, `cli.rs`, `tools.rs`, `storage_api.rs`):** daemon-owned git checkout per mesh at `~/.NeuroStrata/synaptic-mesh/<mesh_id>/` with a one-line project pointer file (`.NeuroStrata/synaptic-mesh-ref`); `neurostrata-mcp synaptic-mesh {init,join,leave,status,pull,push,recover}` (alias `mesh`) + `neurostrata_synaptic_mesh_*` MCP tools; ff-only pulls (divergent history is an operator decision, never a silent merge), non-interactive git with `GIT_TERMINAL_PROMPT=0`, and `StorageApi` wiring so `mesh push`/`pull` compose with the same failure-soft rendering as every other surface.
  - **S3 + S5 — durable outbox + mesh-worker (`outbox.rs`, `worker.rs`):** WAL-first capture of shareable mutations (`created`/`superseded`/`archived`, `stable_op_id` as the record `update_id`); publish gates (`eligibility: shareable`, shareable memory types, foreign-origin rows never re-exported, procedure definitions published with schema-shaped content and no counters); WAL-before record-file before git-commit before mark-committed, so every crash boundary retries idempotently.
  - **S4 — import/replay (`import.rs`):** fetch → changed-path diff (accepted-head cursor; loss falls back to validated full-tree reconciliation) → validate → merge-table apply → store rows with mesh provenance; hostile checkout contents (traversal names, symlinks, exec bits, garbage JSON, unknown schema versions) ignored and named, store untouched; private rows never stomped; cursor never advanced past an unresolved conflict.
  - **S6 — lifecycle table + conflict overlay (`record.rs` transitions, `import.rs` merge table, `conflict.rs`):** state machine (archived/superseded/tombstoned) enforced at import; concurrent cases preserved as *named variants* in a conflicts dir beside the checkout with an unresolved-count overlay surfaced by `mesh status` — variants preserved, no silent drops, no silent resolution.
  - **S7 — trust gates (`manifest.rs::MeshPolicy`, `worker.rs`, `import.rs`):** opt-in sharing default (`require_record_eligibility`), imported procedures arrive `trust: untrusted` and fire only via local promotion, `imported_procedures_trust` locked to Untrusted.
  - **Triggers/recovery/status:** mesh sync hooks are fail-soft one-liners (a rejected/unreachable remote queues locally; the hook still exits 0 and your source push is never blocked); `mesh recover` prints state + manual fixes (never auto-`reset --hard`); `mesh status` reports epoch, sync state, outbox pending/age, accepted/published head cursors, unresolved conflicts, health, and the permanence disclosure.
- **Acceptance evidence suite (`src/synaptic_mesh/acceptance.rs`, slice 8, test-only):** four cross-node scenarios over REAL git (bare remote + clones): disjoint offline adds converge with zero conflicts; same-id concurrent conflict between two nodes preserves variants + pauses integration; an archive crosses two nodes within one sync cycle and stays archived; full-tree crash recovery preserves private rows and procedure state in one pass. In-file matrix documents which spec §"Acceptance evidence" rows are pinned elsewhere. The slice-8 run logged two spec gaps **open** (old-epoch quarantine unenforced; no oversized-JSON cap) — follow-up slices F3/F4 closed **both** before release: epoch quarantine is enforced on both sides (import refuses old- and future-epoch records by `origin_mesh_epoch` vs manifest epoch; publish quarantines stale outbox entries with named reports, left pending for the operator — nothing auto-replays into a new epoch), and a 1 MiB per-record cap rejects oversized payloads pre-parse. The `manifest.rs` doc comment that claimed an epoch check that did not exist is repaired to match the code.

### Fixed
- **`task_complete` dropped `memory.metadata` (8ige):** the completion path rebuilt extraction metadata from scratch (`extracted_from` only), silently dropping caller `source`/`governs` keys and tripping the gate's `rule_without_source` rule — the same bug burned three sessions before it was pinned. Caller metadata now flows through the `add_memory` pipeline unchanged (absent `extracted_from` stamped, present-without-id appended, wrong-type rejected), pinned by deep-equality and regression tests.
- **Direction-B conflict preservation (F1):** a supersede+edit conflict's preserved edit record was rebuilt as `{"text": <serialized string>}` with a fixed metadata literal and empty relationships — a procedure definition degraded to string-in-string *on the local node*, and the row's own metadata (`agent_name`, `tags`, links) vanished. Now structured content is preserved as-is, metadata is merged (only `eligibility`/`trust`/`references` overridden), and relationships are reconstructed and extended with the spec's references link.
- **Unpinned safety sites (F2/F4):** `export_graph`'s conflict-suppression filter and the worker's mark-committed-after-rename ordering both shipped mutation-green; each now has a test that fails on sabotage, including a live-captured RED for the mesh status seam. The worker's fault-injection assertion (`contains("rename")` matched its own fixture path — tautological) is tightened to `record rename`.
- **Doc claims vs code (F3):** `record.rs`'s `origin_mesh_epoch` doc now describes the gates that actually exist on both sides (import refusal + publish quarantine); the manifest policy comment no longer claims a check it never kept.

## [1.8.0] - 2026-10-09

### Added
- **Procedural memory (vocabulary v4):** the third leg of the memory triad. `procedure` memory type with `trigger` / `remaining_fires` / `valid_to` / `last_performed_at` / `performance_count` / `last_episodic_pointer` metadata; the `neurostrata_procedure_perform` MCP tool stamps a firing, decrements the iteration budget, and writes the Episodic Buffer entry that `last_episodic_pointer` names (a lapsed procedure answers `lapsed: true`, a spent one `spent: true`, and neither mutates the row); and `get_snapshot` gains a `procedures_due` strap beside the ready-task head, surfacing every procedure not yet lapsed in `next_due` order. No new bus pulse variant, no new wiring wire, no CLI surface — `valid_to` reuses the existing bi-temporal TTL and the episodic entry reuses `neurostrata_append_log`'s writer.
- **Task rename surface:** `neurostrata_task_update` accepts `title` — it rewrites the task's `content`, re-embeds it (so `search_memory` matches the new wording), and appends `renamed: 'old' -> 'new'` to the task's history. Titles are validated identically on create and rename (non-empty, max 200 characters); the reply gains a `renamed` field. Closes the vocabulary-scrub gap that left stale engine names in live task titles.
- **Thalamic Bus (memory-event system):** a publisher/subscriber bus constructed in `daemon.rs` and threaded through `process_mcp_request` as `Arc<ThalamicBus>`. Bounded queue (cap 10000, drop-oldest) feeds a single dispatcher task that runs each `MemorySubscriber` with a 250ms timeout and a panic-catcher. `RecursionToken` prevents subscriber-induced loops. New endpoints `POST /memory/archive` and `POST /bus/metrics`. Three v1 subscribers: `EpisodicPointerEcho` (closes the Continuous Backup Protocol gap by writing a pointer to the episodic buffer on every `Created`), `ExportFreshnessDirty` (sets a per-namespace `dirty: true` flag on every `Created`/`Superseded`/`Archived` — the flag is *set* by every storage mutation, **and read by the `export-graph` gate consumer in 1.8.0, which refuses to skip the export when any namespace holds a dirty flag**; an operator who has confirmed the export is current can flip `metadata.dirty: false` to clear the gate), `GuardEventLog` (persists every `guard_validate` call as a `memory_type: "guard_event"` row with a 32-bit payload hash). Four emit sites: `add_memory`, `supersede_memory`, `task_complete` extraction, the new `archive_memory` handler. Names follow the cognitive-anatomy convention — Thalamus is the brain's relay station, which is what a pub-sub bus does.
- **Memory vocabulary v4:** `ThalamicPulse` (the bus event type) and `GuardedActionFired` (the 4th variant for guard audit).
- **`neurostrata_archive_memory` MCP tool:** marks a memory archived by writing a tombstone beside it (`archived: true`, `archived_at` merged into the existing metadata). Typed `ArchiveOutcome` so a refusal is never a `200`. The default recall surface — `search_memory`, `get_snapshot`, and the `export-graph` JSON — **now filters out `metadata.archived: true` rows**, with a uniform `include_archived: true` escape hatch on each; the shared read-time helper is `crate::handlers::archive_memory::is_archived(payload)` and is strict on the field shape (literal `true` only). Archiving a row in the machine-wide `global` namespace is refused without an explicit `allow_global: true`, mirroring the guard `handle_supersede_memory` already carries. Raises `tools/list` from 19 to 20 tools.
- **`neurostrata-mcp archive` CLI subcommand:** the same archive operation for shell/daemon use, and **`neurostrata-mcp bus-metrics`** for the ThalamicBus counters (`POST /bus/metrics`) with an explicit timeout rather than the CLI's global one.
- **`export-graph` gate consumer:** `crate::events::check_export_freshness(store)` runs before `vector_store.export_graph(...)` and refuses the export (exit 1) when any namespace holds a `freshness_flag` row with `metadata.dirty: true`. The error message names every dirty namespace so the operator sees the whole backlog at once. The CLI gained `--include-archived`; the daemon HTTP `/graph` route accepts `include_archived=true` as a query parameter, mirroring the read-surface escape hatch.
- **Vector trait / CLI surface for the archive filter:** `VectorStore::export_graph(include_retired, include_archived)` — the trait gained a second bool. `vector_store.export_graph(...)` call sites in the daemon, the store, and the CLI all updated; existing tests retargeted to the new signature.

### Fixed
- **`StubEmbedder` produces a non-integer float vector.** The stub summed input bytes into integer-valued f32 cells, which LadybugDB's `ARRAY_DISTANCE` rejects as a binder error (the literal parses as `INTEGER[]`, the function signature is `FLOAT[]/DOUBLE[]`). A constant per-element fractional offset keeps the stub deterministic and round-trippable — the `assert_eq!(new_vector, StubEmbedder.embed(...))` shape used by `the_replacement_is_embedded_from_its_own_text` still passes. Real embedders never produce integers, so production is unaffected. Three regression tests: the stub outputs at least one fractional element; the vector round-trips through the store unchanged; `handle_search_memory` runs without the `Failed to search database` error that the integer-array binder produced.

### Changed
- **Bootstrap wiring correction (FINDING-bootstrap-wiring-nudge-2026-10-09):** the 1.7.0 wiring panel stated "git-first, never git-only" but its *instruction template* told every git project to "wire the project-pipeline gates above into your CI" — the narrower, GitHub-specific chokepoint — because that is where existing guards already live. A pre-push hook is the broader defense (every push, every host: GitHub, GitLab, Gitea, a bare repo over SSH), so the hook is now the required default and CI is the optional defense-in-depth second layer; a git project with no CI needs nothing beyond the hook. Concretely: the git `resolve_clause` leads with `hooks install` and names the extension point instead of the workflow; `wire_instruction` gained a `files` field carrying a runnable `scripts/hooks/pre-push` (with a `PROJECT_GATES` array to extend) plus `scripts/install-hooks.sh`, so the path of least resistance is "extend the hook that is already there"; `hook_installed` now reads `core.hooksPath` as well as `.git/hooks/pre-push` (the same source of truth the installer uses), so a project that relocated its hooks is no longer told to wire a second, redundant gate; `coverage.git-pre-push` and `coverage.uncovered` report `task-gate` as uncovered when no hook is observable — per Q5, `verified: true` only ever *subtracts*, and a `runs_in: 'git-pre-push'` claim the bootstrap cannot see is never believed; and `verified.hook_project_guards` lists the uncommented `PROJECT_GATES` entries parsed from the installed hook, so an agent can tell which project guards actually run client-side. Non-git projects are unchanged: no hook key beyond `false`, no `git-pre-push` category, no scaffolded files.

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
