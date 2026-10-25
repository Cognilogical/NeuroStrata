# 📓 NeuroStrata Changelog

All notable changes to the NeuroStrata project will be documented in this file.

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
  - On the live graph this recovers 100 sibling pairs that recall could not previously reach (20 in `NeuroStrata`, 80 in `keywest.health`) — measured on 2026-10-20, the day this entry was written; the live graph grows and the definition of a "sibling pair" is not stated here, so the number is a point-in-time measurement rather than a current figure.
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
