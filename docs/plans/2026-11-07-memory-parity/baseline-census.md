# Task 0.1 — Feature-Baseline Census of Fork `develop`

Anthropic-Memory Parity program, Milestone 0. This census records, primitive by
primitive, what exists on the fork's `develop` base that the rest of the plan
(Tasks 0.2–4.3) builds on, with file:line evidence taken from that exact tree.
Upstream facts are recorded only where separately evidenced from the upstream
tree and are labelled **[upstream]** wherever they appear.

## Header — base reconciliation

- **Census base:** fork `develop` @
  `5b8f9a7c3b7808da1f0c550e8eb081667335954c` ("fix(cli): send the auto-spawned
  daemon's stderr to daemon.log instead of /dev/null (#27)" — the PR #27 squash
  merge). Verified live at census time: `git fetch origin develop` followed by
  `git rev-parse origin/develop` returned exactly this SHA, and the census
  worktree HEAD was verified identical before any evidence was taken.
- **Plan/seed SHA discrepancy, reconciled:** the plan and context seed named
  `6ce7021` as the develop tip. That SHA is **not** the tip on this base; it is
  an ancestor exactly 4 commits back (verified:
  `git merge-base --is-ancestor 6ce7021 5b8f9a7…` → true;
  `git rev-list --count 6ce7021..5b8f9a7…` → 4). `6ce7021` is the 2026-11-01
  binaries-drop commit ("chore(repo): drop vendored release binaries from the
  tree"). The four intervening commits are:
  - `d2f87a1` — #25 (T16): one version fact (core + GUI manifests) with a
    standing CI sync check; cxx pin documented
  - `48bf626` — #24 (T15a): quality gates (report-only), 3-OS tests, GUI build,
    scoped release permissions with provenance
  - `c7d8f20` — #26: remove manual dispatch so attested provenance names the
    built tag
  - `5b8f9a7` — #27 (T19): auto-spawned daemon's stderr to daemon.log
- **Cargo version on this base:** `1.14.3` — `Cargo.toml:3`
  (`version = "1.14.3"` under `[package] name = "neurostrata-mcp"`).
- **Census date:** 2026-11-07.
- **Method:** source census only — `grep` / `git show` / `git rev-list` /
  `git diff` against the worktree tree at `5b8f9a7…` (and, for the labelled
  upstream facts and the divergence re-derivation, against fetched upstream
  objects). No binary was built or run; no daemon or store was touched.

## Main table — primitive by primitive

Verdicts are for **this base** (`develop` @ `5b8f9a7…`) unless a row says
otherwise.

| # | Primitive | Verdict | Evidence (this base) | Notes |
|---|-----------|---------|----------------------|-------|
| 1 | Judgment trait surface (`JudgmentProvider`) | PRESENT (duplicate-only) | `src/judgment/mod.rs:33-47` | Exact trait methods: `judge_duplicates` (`mod.rs:36-40`), `health_check` (`mod.rs:43`), `name` (`mod.rs:46`). **No contradiction method exists** — pre-registered result confirmed. Contradiction detection is new work (plan Task 2.7), not an existing primitive. |
| 2a | Active dedup provider per mode — wiring/selection | PRESENT (see per-mode rows) | Selection: `src/judgment/mod.rs:90-123` (`select_dedup_providers`); mode enum: `src/config.rs:11-17`; default mode `Auto`: `src/config.rs:104-113` | Modes: `Auto`, `Heuristic`, `LayaLocal`, `Jev`, `None`. See rows 2b–2f. |
| 2b | Mode `Auto` (the default) | PRESENT — Mock active, Mock-as-Laya shadow | `src/judgment/mod.rs:96-101` (active = `MockJudgmentProvider`, shadow = the passed-in Laya candidate); the candidate actually passed is Mock: `src/judgment/mod.rs:140-146` | Default config therefore runs the mock as both active and shadow judge. |
| 2c | Mode `Heuristic` | PRESENT — Mock active, no shadow | `src/judgment/mod.rs:102-106` | Despite the mode name, the "heuristic" provider instantiated on this base is `MockJudgmentProvider`. There is **no** `HeuristicJudgmentProvider` type anywhere on this base. |
| 2d | Mode `LayaLocal` | DIVERGED (name vs. reality) | `src/judgment/mod.rs:107-111` (active = passed-in Laya candidate, error if none); `src/judgment/mod.rs:134-146` | `DeduplicationChecker::new` passes `MockJudgmentProvider` into the Laya slot, with the in-code explanation at `mod.rs:134-137`: the Laya provider "is not functional in this build … so the Laya slot carries the heuristic MockJudgmentProvider … in LayaLocal mode the heuristic judges, and in Auto mode it shadows." So in `LayaLocal` mode the **Mock** is the active judge. |
| 2e | Mode `Jev` | PRESENT — real provider wired | `src/judgment/mod.rs:112-116`; `src/judgment/mod.rs:143-145` constructs `TypeSafeJevProvider`; provider type at `src/judgment/typesafe_jev.rs:9`, trait impl at `typesafe_jev.rs:112-159` | The only non-mock provider actually wired into the checker on this base. |
| 2f | Mode `None` | PRESENT — dedup disabled | `src/judgment/mod.rs:117-120` | `(None, None)` provider set; baseline write path allowed. |
| 2g | `MockJudgmentProvider` behaviour | PRESENT | `src/judgment/provider.rs:9`, impl `provider.rs:18-36` | Always returns an empty duplicate list ("not a duplicate" — safe fallback, `provider.rs:24-26`); `health_check` returns `Ok(true)`; `name()` = `"mock"`. It detects no duplicates at all. |
| 2h | Real Laya/ONNX provider (`LayaOnnxProvider`) | PRESENT as a type, NON-FUNCTIONAL, NOT WIRED | Type: `src/judgment/laya_onnx.rs:23`; `load` at `laya_onnx.rs:36` sets `RuntimeState::Unavailable` (`laya_onnx.rs:58-60`); trait impl `laya_onnx.rs:101-124`: `judge_duplicates` returns `Err("Laya ONNX inference is not available in this build")`, `health_check` returns `Ok(false)` | The fork's honest-Unavailable semantics. Nothing in `DeduplicationChecker::new` constructs it (contrast row 2d). No other judgment provider types exist on this base (no Heuristic provider — see row 2c). |
| 2i | Shadow-verdict machinery | ABSENT | `src/judgment/mod.rs:82-85` (`ProviderSet { active, shadow }` fields exist), but `check_duplicates` (`mod.rs:165-232`) invokes only the active provider and never calls or records the shadow | The `shadow` slot is populated in `Auto` mode (row 2b) but its verdict is never computed or stored on this base. **[upstream]** `8fdae4b` records shadow verdicts: `last_shadow` field and its "kept, never discarded" contract at upstream `src/judgment/mod.rs:133-135`, shadow invocation/recording in upstream `check_duplicates` at `mod.rs:218-259`. |
| 2j | **[upstream]** provider internals at target `8fdae4b` | DIVERGED from this base | Evidenced via `git show 8fdae4b:…`: version `1.16.1` at upstream `Cargo.toml:7`; `HeuristicJudgmentProvider` defined at upstream `src/judgment/provider.rs:14` (impl `:17`, `name()` `:89`) and selected as active for `Auto` and `Heuristic` at upstream `src/judgment/mod.rs:101` and `:107`; upstream `DeduplicationChecker::new` (upstream `mod.rs:139`ff.) attempts a real `LayaOnnxProvider::load` from the bundle path and continues with `None` (warn-logged) on failure | So the M0 sync changes judging behaviour behind plan Task 2.6's sweep, exactly as RC4 pre-registered: same trait surface, different provider internals. Upstream facts only — none of this is on the census base. |
| 3a | Thalamic Bus — event (pulse) vocabulary | PRESENT | `src/events/event.rs:19-63` (`ThalamicPulse`, `#[non_exhaustive]`) | Exactly four variants: `Created` (`event.rs:24-28`), `Superseded` (`:30-34`), `Archived` (`:36`), `GuardedActionFired` (`:55-62`). No consolidation event class exists (plan Task 2.1 adds one). |
| 3b | Thalamic Bus — durable event classes | PRESENT (exactly two) | `src/events/outbox.rs:73-78` (`EventClass::{Audit, Mesh}`); `EventClass::all()` returns `[Audit, Mesh]`: `outbox.rs:82-84`; re-exported at `src/events/mod.rs:19` | No third class. Class↔subscriber mapping: `outbox.rs:97-103` (`guard_event_log` → Audit, `mesh_outbox` → Mesh; every other subscriber is never durability-qualified). |
| 3c | Durable outbox sequencing | PRESENT (single cross-class sequence, qualified pulses only) | Format/sequence contract: `src/events/outbox.rs:30-31` ("`seq` is a single sequence across both classes"); `seq` assigned at append: `outbox.rs:393-415`; qualification rule: `src/events/bus.rs:415-429` (`qualified_classes`); append-before-dispatch: `bus.rs:440-475` (`append_qualified`), called from `emit`/`emit_checked` at `bus.rs:531` / `:562` | A pulse receives a durable seq only if it **qualifies**: an outbox is attached, the class's named subscriber is registered, and the class is interested in the pulse (`bus.rs:415-429`). Interest: Audit ⇔ `GuardedActionFired` only (`outbox.rs:113-115`); Mesh ⇔ `Created`/`Superseded`/`Archived` (`outbox.rs:116-122`). |
| 3d | Every memory mutation emits a sequenced event | ABSENT (pre-registered NO, confirmed) | **(a) Bus seq qualification (`event-outbox.jsonl`):** `qualified_classes`: `src/events/bus.rs:415-429` — a class qualifies iff an outbox is attached, its named subscriber is registered, and the class is `interested_in` the pulse; Mesh interest: `src/events/outbox.rs:111-122` — Mesh is interested in **all** `Created`/`Superseded`/`Archived` pulses **regardless of memory type or shareability**. Seq assignment is therefore type-agnostic: a non-shareable `Created` pulse *can* receive an event-outbox seq. **(b) Mesh-WAL capture (a separate mechanism):** `src/events/subscribers/mesh_outbox.rs:11-13` (module doc: "It captures only the shareable types (`rule`, `fact`, `procedure`)"), `mesh_outbox.rs:167-175` (`entry()` returns `None` for auto-ingestor rows, `::`-qualified structural ids, and non-shareable types); shareable set at `src/synaptic_mesh/outbox.rs:114` (`SHAREABLE_MEMORY_TYPES = ["rule", "fact", "procedure"]`). This filter gates only whether `MeshOutbox` writes a mesh-WAL entry — it does **not** gate seq assignment in (a). | The ABSENT verdict rests on three grounds, none of them the shareable filter: (1) seq qualification is **conditional** — no outbox attached or no named subscriber registered ⇒ no seq (row 3c; `bus.rs:415-429`); (2) a `procedure_perform` firing emits **no pulse at all** (documented in the test at `mesh_outbox.rs:498-505`), so that mutation class can never be sequenced; (3) the pulse vocabulary (row 3a) has **no variants for other change kinds** (e.g. metadata-only updates), so those mutations emit nothing. Separately, at the mesh-WAL layer (b), non-shareable/private mutations (tasks, structural rows, all other types) are not captured for mesh export — a sharing-boundary fact, distinct from sequencing. Emit sites that do exist: `src/server.rs:1899-1900` (Created, `emit_checked`), `src/server.rs:2782-2783` (Superseded, `emit_checked`), `src/handlers/archive_memory.rs:160-163` (Archived, `emit_checked`), `src/task/mod.rs:1485-1486` (Superseded, `emit_checked`), plus `emit` sites in `src/daemon.rs` and `src/server.rs` for `Created`/`GuardedActionFired`. |
| 4 | Episodic buffer — retention/pruning | PRESENT | Module doc: `src/buffer.rs:1-5`; rollover threshold `ROLLOVER_BYTES = 500 * 1024`: `buffer.rs:15`; default retention 30 days: `buffer.rs:16-17`; config read (`buffer_retention_days`, `episodic_buffer` default-enabled): `buffer.rs:24-42`; append path `append_entry`: `buffer.rs:117`, rollover + prune call at `buffer.rs:173-182`; `prune_rolled`: `buffer.rs:235-257` | Behaviour: `neurostrata_append_log` appends timestamped entries to `<project_root>/.NeuroStrata/sessions/current.md`. Pruning is triggered **only by a rollover** during an append (when `existing + entry.len() > max_bytes`, the file is renamed to `session-<timestamp>.md` and `prune_rolled` runs — `buffer.rs:181-182`). `prune_rolled` deletes rolled `session-*.md` files whose modification age exceeds `retention_days * 24h` (cutoff at `buffer.rs:237`). There is no time-based/background pruning and no count-based cap: if no rollover occurs, nothing is pruned, and `current.md` itself is never pruned. |
| 5a | Search — retrieval shape on this base | PRESENT (one vector path + graph expansion) | `search_inner`: `src/store/ladybug.rs:781`; Step 1 base vector search (LadybugDB `array_distance` over `m.embedding`, `ORDER BY dist ASC`): `ladybug.rs:816-848`; Step 2 "Hybrid GraphRAG — directed evidence expansion": `ladybug.rs:972`ff. Store trait surface: `src/traits.rs:325` (`search`), `traits.rs:495` (`search_with_domain`) | One retrieval engine: vector distance in the store, followed by graph-edge expansion inside the same routine. No second (lexical) retrieval surface feeds it on this base. |
| 5b | `src/search.rs` | PRESENT — but only as a filter/renderer helper (see Flag F1) | Whole file is 88 lines: `src/search.rs:1` ("Upgraded search filters and compact renderer helpers"); `SearchFilters`: `search.rs:6-12`; filter predicates `search.rs:24-88` | It contains **no** index, query engine, or ranking. It is not a lexical/BM25 surface. (Plan Task 3.4 says "Modify: `src/search.rs` (post-sync shape)" — the post-sync shape is a different, larger module; see row 5e.) |
| 5c | `src/fts` (Tantivy FTS module) | ABSENT | No `src/fts` path exists in the tree at this base (full `src/` listing taken at census time); zero `tantivy` hits in `src/`, `Cargo.toml`, or `Cargo.lock` | Pre-registered ABSENT confirmed. **[upstream]** present at `8fdae4b` as `src/fts/mod.rs`. |
| 5d | BM25 / lexical retrieval surface | ABSENT | Zero `bm25` hits in `src/`, `Cargo.toml`, `Cargo.lock`; no `src/lexical.rs` (row 10) | **[upstream]** `8fdae4b` has `src/lexical.rs`, whose header (via `git show`) describes it as the "Tantivy lexical full-text index", `tantivy = "0.27"` in its manifest. |
| 5e | Three search surfaces / fused modes | ABSENT on this base | Rows 5a–5d | The "three surfaces" the plan's Phase 2 fuses exist only post-sync. **[upstream]** at `8fdae4b` the search-related paths are `src/search.rs`, `src/fts/mod.rs`, `src/lexical.rs` (`git ls-tree`). |
| 6 | `include_ast_edges` | ABSENT on this base (DIVERGED vs. upstream) | Zero hits for `include_ast_edges` or `include_ast` anywhere in `src/`, `tests/`, or `docs/` at this base | The nearest on-base relative is a different switch: `SearchFilters.include_code_ast` (`src/search.rs:11`), which only controls whether `code_ast`-type memories pass the type filter (`search.rs:28-30`). **[upstream]** `include_ast_edges` is PRESENT at `8fdae4b`: tool-schema entry at upstream `src/server.rs:280` ("Also bundle 1-hop related memories (DEPENDS_ON/SUPERSEDES/RELATES_TO) and a compact AST neighborhood summary… Default true") and argument handling at upstream `src/server.rs:2007-2011`. |
| 7 | Snapshot contents (`handle_get_snapshot`) | PRESENT | Handler: `src/server.rs:2011`; tool registered as `neurostrata_get_snapshot`: `server.rs:154`, dispatched `server.rs:642-643`; final assembly: `server.rs:2381-2396` | **Includes:** (1) the task head / Zero-Action Start prefix (`crate::task::snapshot_prefix`, computed at `server.rs:2091` — pre-truncation); (2) hook-hygiene line when non-empty (`server.rs:2382-2384`); (3) a doctor report when one was produced (`server.rs:2302`, `:2356`, pushed at `:2385-2387`); (4) the `procedures_due` strap (`server.rs:2388`; strap builder `src/tools/procedure.rs:172`); (5) the registered-domains line (`server.rs:2389`); (6) the domain strap — up to 3 domain-tagged memories, per-domain cap 2 in `recent_domain` mode (`server.rs:2130-2260`); (7) the body: the **top 5** memories by `access_count` descending, pretty-printed JSON (`server.rs:2100-2118` sort, `:2373` `truncate(5)`); (8) an unmigrated-ids migration notice when applicable (`server.rs:2394-2396`). **Excludes by default:** archived/tombstoned rows (retained out unless `include_archived: true` — `server.rs:2068-2072`), mesh-conflicting rows (`server.rs:2075`), the namespace registry row itself, and rows whose `valid_to` has passed. It is a bounded operator view (top-5 + straps), **not** a full-namespace dump, and it has no token-budget parameter (plan Task 4.1 adds `injected_token_budget`). |
| 8a | `src/rebac.rs` | ABSENT | No such path in the tree at this base | Pre-registered check confirmed. **[upstream]** present at `8fdae4b` (`git ls-tree`). |
| 8b | `src/did.rs` | ABSENT | No such path in the tree at this base | **[upstream]** present at `8fdae4b` (`git ls-tree`). |
| 9a | Supersede operation | PRESENT | Tool `neurostrata_supersede_memory` registered `src/server.rs:242`, dispatched `server.rs:679-680`; handler `handle_supersede_memory`: `server.rs:2452`; emits `ThalamicPulse::Superseded` via `emit_checked`: `server.rs:2782-2783`; task-completion supersede path: `src/task/mod.rs:1485-1486` | Known-PRESENT confirmed. |
| 9b | Archive operation | PRESENT | Typed core `archive_memory`: `src/handlers/archive_memory.rs:94`; prose wrapper `handle_archive_memory`: `archive_memory.rs:184`; tool `neurostrata_archive_memory` registered `src/server.rs:481`, dispatched `server.rs:807-808`; writes `metadata.archived: true` tombstone (`archive_memory.rs:49` doc, `:94`ff.); emits `ThalamicPulse::Archived` via `emit_checked`: `archive_memory.rs:160-163` | Archive is a tombstone, not a delete; read surfaces hide tombstoned rows by default. |
| 10a | `src/fts` | ABSENT | See row 5c | Pre-registered ABSENT set — each item verified individually against the tree listing and by path test. |
| 10b | `src/lexical.rs` | ABSENT | No such path at this base | **[upstream]** present at `8fdae4b`. |
| 10c | `src/rdf` | ABSENT | No such path at this base (no `src/rdf` file or directory) | — |
| 10d | `src/community.rs` | ABSENT | No such path at this base | — |
| 11a | Memory→transcript citation mechanism (plan Task 2.3 substrate) | ABSENT | Provenance today is the free-form `metadata.source` field: `MemoryPayload` is `src/traits.rs:57-67` (metadata is an untyped `Value`); `has_source`: `traits.rs:82-87` — a memory counts as sourced iff `metadata.source` is a non-empty string **or** any non-null structured value (shapes exercised in tests at `traits.rs:889-891`, e.g. `{kind, ref, captured_at}`); the supersede tool documents the same semantics at `src/server.rs:255` | There is no citation type, no log-range reference, and nothing that resolves a memory back to a transcript/log span. `source` is provenance text/metadata, not a retrievable citation — exactly the pre-registered expectation. Task 2.3's citations are new substrate. |
| 11b | `access_count` field | PRESENT | Initialised to `0` in metadata on add: `src/server.rs:1474` (and on supersede: `server.rs:2624`); store trait: `src/traits.rs:475` (`increment_access_count`), `traits.rs:486` (`increment_access_counts`); incremented **only for direct search hits**: `server.rs:2950-2966`; consumed as a ranking signal in graph reads: `src/graph_read/memories_for_file.rs:73`, `:129-140` | Pre-registered PRESENT confirmed. It is a metadata field, not a store column. |
| 11c | `last_accessed` timestamp | ABSENT | Zero hits for `last_accessed` in `src/` and `tests/` at this base | Pre-registered ABSENT confirmed. Plan Task 4.2 adds it ("`access_count` exists; the other two inputs don't" — Feasibility F10). |
| 11d | Per-anchor churn signal (plan Task 4.2 input) | ABSENT | No churn signal exists: the only `churn` hits in `src/` are an unrelated guard rule-class string (`src/server.rs:458`, `'churn-reject'`) and none in the store/search paths | Third Task 4.2 input, absent like `last_accessed`. |
| 11e (keying) | Span-hash **keying** of symbols | PRESENT | `symbol_id(path, kind, name, span_sha256)` formats a symbol's logical id as `path#kind:name@hash`, the final segment being the SHA-256 of the symbol's full definition text: `src/parser/generation.rs:100-107`; the hash is computed at ingest (`src/parser/ingest.rs:472`) and stored in symbol metadata (`ingest.rs:621-622`) | Keying only: a changed body hash produces a different symbol id, so re-ingest upserts unchanged symbols and treats edited ones as new (doc at `generation.rs:100-104`). This is identity formatting/storage, **not** a lookup (see next row). |
| 11e (lookup) | **By-span-hash lookup** — resolve a symbol/memory by its span hash alone (the primitive plan Task 2.4 anchor re-resolution relies on) | ABSENT | No query by span hash exists: `git grep span_sha256` over `src/` finds it only (i) formatting the id (`src/parser/generation.rs:105-107`), (ii) stored in metadata (`src/parser/ingest.rs:621-622`), and (iii) compared stored-vs-current for freshness (`src/context/freshness.rs:15-17`, `:55-66`, `:90`). The nearest matcher, `is_symbol_of` (`src/store/ladybug.rs:5991-6002`), matches on path + name and **discards the hash** — `name_at_hash.split('@').next()` at `ladybug.rs:6001` — and its only caller is location-scoped (query by `m.location = path`, filtering via `is_symbol_of` at `ladybug.rs:6185`) | Task 2.4 therefore **adds** the by-span-hash lookup; it does not reuse an existing one. What it can build on is the keying above (hash already embedded in every symbol id and stored in metadata) plus the stored-vs-current hash comparison in `src/context/freshness.rs`. |
| 11f | Wiki namespace / page machinery (plan Tasks 3.1–3.3, 3.5) | ABSENT | Zero `wiki` hits in `src/**/*.rs` at this base; no `src/wiki/` directory | No `wiki:<project>` namespace handling, no page leases, no page store. Entirely new in Phase 2. |
| 11g | RRF / rank fusion (plan Task 3.4) | ABSENT | Zero hits for `tantivy`, `bm25`, `RRF`, or `reciprocal` (case-insensitive) in `src/`, `Cargo.toml`, `Cargo.lock` at this base | `fuse(results_per_surface, k=60)` and `src/search/fusion.rs` are new work; there is also only one surface to fuse on this base (row 5e). |
| 11h | Consolidation engine surface (plan Phase 1: `src/consolidation/`, watermark, consolidation event class) | ABSENT | No `src/consolidation/` directory; zero hits for `ConsolidationWatermark` in `src/`; event classes are Audit/Mesh only (row 3b) | The one `consolidation` mention in the store is unrelated: an ingest `EXTRACTED_FROM` edge comment at `src/store/ladybug.rs:5780`. Everything in Tasks 2.1–2.12 is new surface on this base. |

## Flags — where the tree contradicts or sharpens the pre-registered expectations

- **F1 — `src/search.rs` EXISTS on develop.** The checklist phrasing ("whether
  `src/search.rs` … exist on develop") resolves to: the file exists, but it is
  an 88-line filter/renderer helper (`src/search.rs:1-88`), not a search
  surface. The substantive expectation — one vector retrieval path in
  `src/store/ladybug.rs`, no lexical surface — holds (rows 5a–5e). Any reader
  who treats "`search.rs` exists" as "a second search mode exists" would be
  wrong on this base.
- **F2 — `LayaLocal` mode does not run Laya.** On this base the Laya slot is
  filled by `MockJudgmentProvider` inside `DeduplicationChecker::new`
  (`src/judgment/mod.rs:140-146`), so `Auto` (active **and** shadow),
  `Heuristic`, and `LayaLocal` all judge with the mock, which returns "no
  duplicates" unconditionally (`src/judgment/provider.rs:24-26`). Effective
  dedup judgment on stock develop is therefore a no-op except in `Jev` mode.
  This matches the plan's RC4 statement; it is flagged here because the mode
  names alone would mislead a census reader.
- **F3 — No contradiction anywhere in the trait, as pre-registered.** Row 1 is
  a confirmation, not a surprise: `judge_duplicates` is the only judgment
  method on either base (the upstream trait at `8fdae4b` has the identical
  three-method surface, upstream `src/judgment/mod.rs:33-46`).

No other checklist item contradicted its pre-registered expectation. Every
mandatory checklist item (1–11) was evidenced from the tree; none is missing.

## Divergence re-derivation (run at census time, 2026-11-07)

Method: `git fetch upstream` (read-only) in the main clone, then `git rev-list
--count` and `git diff --name-only` between the census base and each upstream
ref. Refs resolved at run time:

- census base: `5b8f9a7c3b7808da1f0c550e8eb081667335954c` (= `origin/develop`)
- target (a): `8fdae4b745269b4097ecbf02a6375f0a0c3b792c` (PR #57; the plan's
  SHA-pinned sync target)
- live tip (b): `581ae333a79460497be904c24122b93ecbead58c` (= `upstream/main`
  at fetch time; PR #61 merged on top of the target — the only commit in
  `8fdae4b..581ae33` is `581ae33` "test(harness): read request before
  responding in federation tests; raise daemon probe budget (#61)",
  test-harness only)
- merge-base of base and target: `7d836c72e81552523b86abbb0b80d71709551c4e`

### (a) Target `8fdae4b`

```
$ git rev-list --count 5b8f9a7c3b7808da1f0c550e8eb081667335954c..8fdae4b
773
$ git rev-list --count 8fdae4b..5b8f9a7c3b7808da1f0c550e8eb081667335954c
538
$ git diff --name-only 5b8f9a7c3b7808da1f0c550e8eb081667335954c 8fdae4b -- src | wc -l
154
$ git diff --name-only 5b8f9a7c3b7808da1f0c550e8eb081667335954c 8fdae4b | wc -l
430
```

- Commits in target not in develop: **773**
- Commits in develop not in target: **538**
- Differing `src/` files: **154** (430 differing files in total)

### (b) Live upstream tip `581ae33`

```
$ git rev-list --count 5b8f9a7c3b7808da1f0c550e8eb081667335954c..581ae33
774
$ git rev-list --count 581ae33..5b8f9a7c3b7808da1f0c550e8eb081667335954c
538
$ git diff --name-only 5b8f9a7c3b7808da1f0c550e8eb081667335954c 581ae33 -- src | wc -l
154
$ git diff --name-only 5b8f9a7c3b7808da1f0c550e8eb081667335954c 581ae33 | wc -l
430
```

- Commits in live tip not in develop: **774** (the target's 773 + PR #61)
- Commits in develop not in live tip: **538**
- Differing `src/` files: **154** (430 differing files in total — unchanged
  from (a), consistent with PR #61 being test-harness only and touching no
  `src/` file relative to the base diff)

These counts are two-dot `rev-list` counts (all reachable commits unique to
each side since the merge-base), re-derived at run time as Task 0.1 requires —
not carried over from the plan or seed.
