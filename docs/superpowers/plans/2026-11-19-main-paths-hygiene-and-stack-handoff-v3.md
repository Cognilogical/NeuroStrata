# Main Paths Hygiene and Stack Handoff Plan (v3, final)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> **Supersedes v1** (`2026-11-18-paths-relative-remediation.md`, killed by the wringer panel: stale base `10debfdb`, tasks deciding unruled policy, beads-as-evidence) **and v2** (`2026-11-19-main-paths-hygiene-and-stack-handoff.md`, rejected by the independent reviewer: test contracts too weak to prove the ACs, an unapproved public push in setup, and a fourth "defect" that is already fixed and tested on main). Every revision the reviewer required is applied here.

**Goal:** Fix the three path-related defects that exist on merged main `cda06dce`, verify the fourth is already covered by a passing test, and hand the stack-only findings to their author and the policy questions to the maintainer — with every public write bound to a recorded user approval.

**Architecture:** **Part A** is code on one local branch off `cda06dce` (`fix/paths-main-hygiene`), delivered as one upstream PR from our fork, only after the approval ledger records the user's sign-off for the fork push and the PR: portable implicit rule source, char-safe evidence-id slice, one-scan-set batching inside `memories_for_file_forms`, and a verification-only run of the existing root-prefix test. **Part B** is two local draft packages — review comments for #279–#282 and one maintainer comment for #275 — each posted only after its recorded approval. Nothing in this plan edits the author's branches or PR bodies.

**Tech Stack:** Rust (edition 2021, bin target `neurostrata-mcp`), Kùzu/LadybugDB store, serde_json, tokio; `cargo test --locked --bin neurostrata-mcp <filter> -- --test-threads=1`, one substring filter per invocation; the repo's local gate CLI `./target/debug/neurostrata-mcp task validate|gate`; gh CLI (read-only until an approved post).

**Spec:** Merged main `cda06dce` (ground truth; re-verified 2026-11-19 after #300 merged — none of the Part A items was fixed by it); re-confirm the head before every task). Wringer evidence: `/tmp/ns-panel/wringer-feasibility.md`, `wringer-risk.md`, `wringer-devils-advocate.md`, `wringer-independent.md`. [Issue #275](https://github.com/Cognilogical/NeuroStrata/issues/275) — it has exactly **three** open questions: (1) competing roots ("should the registered root win?"), (2) refusing outside paths — the author's stack comment ties #282 to this one, (3) `doctor` versus a separate CLI; the post-#283 stack-fate question is **new**, not the issue's question 4. The earlier panel review (comment 6994409026) is leads, not spec; its wrong export citation is `server.rs:5137` at `796f7003` (equivalent `:5178` at `10debfdb`) — the real export reader at main is `src/store/ladybug.rs:12246` → `source_context::export_absolute_path`.

## Global Constraints

- Conventional Commit subjects; one local branch `fix/paths-main-hygiene` off `cda06dce`; one upstream PR from our fork; no stacked-layer ambiguity.
- **Approval ledger:** `/tmp/ns-panel/approval-ledger.md`. Every public write — fork branch push, upstream PR open, each `gh pr review`, the issue comment — requires a ledger entry recording destination, operation, exact content file and content SHA-256 (or branch SHA), and the user's in-session approval before it happens; the resulting URL/SHA is recorded after. With no approval, the completion state is: local branch, local drafts, recorded outcomes — nothing public.
- Upstream `main` ruleset: `build (ubuntu-latest)`, `test (ubuntu)`, `gate-receipt`, 1 approval, last-push approval, thread resolution. `test (ubuntu)` also runs wasm tests and the remediation self-test (`ci.yml:195-201`). A ruleset failure is a stop.
- **Protocol preflight (before any edit):** run `./target/debug/neurostrata-mcp task validate` in the repo and record its report. The session has no `neurostrata_task_*` MCP tools, so task claiming and memory extraction cannot be performed here; the limitation is recorded in the ledger, the advisory `task validate` runs before every user-approved push, and the strict gate runs before the final one. Upstream review remains the real gate.
- No public write without a ledger entry: no `quest_helper.js sync` (posts a public issue comment), no `close` (marks every bead COMPLETED unconditionally — helper `:539-555`), no PR-body edits on the author's PRs, no pushes to `anthomus/NeuroStrata`.
- No timing assertions in any **new** test this plan adds. Pre-existing timing gates elsewhere in the suite (e.g. `src/retention/gate.rs:239-250,296-312`) are out of scope and stay untouched.
- Scan and query counts are asserted through `cfg(test)` counters placed at the physical query sites.

## Review Focus

1. A shareable rule with an absolute `location` and no explicit `source`, across the full input matrix: peers must never receive the path, and the emitted source must be the exact specified value. (Task 2)
2. Metadata whose hash places a multibyte character across byte 12: the persisted evidence ID must be the exact expected string, never a panic. (Task 3)
3. Two spellings of one logical file in one request: one physical scan set, complete return contract preserved. (Task 4)
4. Root-prefix equality is already covered on main: verification, not implementation. (Task 5)
5. A migrated namespace that later receives an absolute-path write: a maintainer question with corrected evidence, not our code. (Task 8)

## Test Plan

- **Per-task:** `cargo test --locked --bin neurostrata-mcp <filter> -- --test-threads=1`, one substring filter per invocation; baseline red and final green recorded per test in the ledger with the base SHA.
- **Full gate before each commit:** `cargo test --locked -- --test-threads=1`.
- **Eval suite:** Task 6 owns registering `paths-main-hygiene` in `eval/registry.json` with the exact final fully qualified test names from Tasks 2-4 and the existing root-prefix test from Task 5, thresholds `ignored: 0`, then running `scripts/remediation-eval.sh paths-main-hygiene` from a clean checkout of the final commit. The runner's tracked-dirt refusal is why every Part A task commits before the suite runs.
- **Sentinel checks (Task 2):** a fixture absolute root containing the literal `sentinel`; grep the emitted mesh record and the N-Quads text.
- **Counters (Task 4):** `cfg(test)`-only counters at the three physical query sites in `src/store/ladybug.rs` (`:12792` exact-file, `:12809-12814` directory, `:12865-12877` metadata).

## Acceptance Criteria

- **AC1:** a test drives the rule input matrix — absolute POSIX and Windows `location`, missing source, blank source, absent location, usable relative input, escaping relative input — through a successful `add_memory`, asserts the record ID, the exact emitted `metadata.source` value and the exact RDF source triple (`dataset_canonical.rs:72-74` omits the triple when source is absent — assert presence), and greps both outputs for `sentinel`. The baseline failure is pinned to the leaked source at `server.rs:4729`. Explicit caller-supplied `source` is preserved unchanged.
- **AC2:** a fixture seeds one resolvable declaration with a known file, line range and a supplied hash whose byte 12 falls inside a multibyte character; the store persists the edge and its evidence ID is exactly the expected string (e.g. `src/a.rs:3-4:abcdefghijk`). A long ASCII hash still yields its original first 12 bytes. Baseline: panic at the slice.
- **AC3:** `cfg(test)` counters at the three physical query sites, unit = one `memories_for_file_forms` call for one logical file. Two distinct normalized spellings with discriminating hits: baseline (2,2,2), final (1,1,1) with ancestors enabled; (1,0,1) with ancestors disabled; (0,0,0) with no forms; duplicate spellings collapse to one. The benchmark's own form counter (`server.rs:21728-21741` expects four form operations for two logical files) is explicitly retained or relabeled, never silently broken.
- **AC4:** the full return contract is compared against pinned per-form behavior: ordered hits, match kinds, explanations, linked-file counts, bridges, and the OR-ed truncation flag; a form above the 1,024 cap and two individually under it whose union exceeds it; mixed success (successful forms retained when another fails), all-forms-error (first error returned), empty input; shared-query failure falls back to per-form queries (retry on batch failure only, scan bounds counting successful first attempts).
- **AC5:** Task 5 ends in exactly one recorded outcome: the existing test `paths::tests::prefix_equal_to_configured_or_checkout_root_resolves_to_root_forms` (and its module siblings, if the pinned CI log names a second) run with `--exact` at the base SHA, with command and output recorded in the ledger. Implementation reopens only if a distinct failing case with stated expected forms is found.
- **AC6:** the eval suite is registered by Task 6 with the exact test names of Tasks 2-5; on a clean checkout of the final commit it exits 0 with `ignored: 0`, and the recorded expected-test list matches what ran. Distinguished in the ledger from the upstream ruleset checks required on the PR.
- **AC7:** Part B produces exactly five local files — `/tmp/ns-panel/review-comments-279.md`, `-280.md`, `-281.md`, `-282.md`, and `/tmp/ns-panel/maintainer-question-275.md` — each meeting its required-topics list below; every code citation pinned to repo/SHA/file/line, every policy citation to issue headings, comment permalinks or PR events; draft content SHA-256 and each PR head re-verified immediately before any approved posting, one `gh pr review <n> --comment --repo Cognilogical/NeuroStrata --body-file <file>` per PR and one issue comment if approved. Zero public writes exist without a matching ledger entry scoped to this run's actor, operations and time window.
- **Scope ruling:** Part A fixes defects on main and closes no issue. Part B hands findings and decisions to their owners; it does not close #275, edit any PR body, or implement the migration. Audit at the end: base-to-head diff contains only the agreed files; zero issue closes, zero foreign-branch writes, zero PR-body edits, zero migration implementation by this run.

---

### Task 1: Local setup and protocol preflight (local-only)

**Files:** none committed; setup plus ledger creation.

**Interfaces:** Produces branch `fix/paths-main-hygiene` off `cda06dce` and the approval ledger. No push in this task.

- [ ] **Step 1:** `git fetch cognilogical main`; confirm `cognilogical/main == 8cc2dc2f`. If it moved, stop and re-verify every anchor in this plan against the new head.
- [ ] **Step 2:** Create a local worktree and branch off `cognilogical/main`. Do not push anywhere.
- [ ] **Step 3:** Run `./target/debug/neurostrata-mcp task validate`; record its report in the ledger, plus the session's limitation (no `neurostrata_task_*` tools: no task claim, no memory extraction).
- [ ] **Step 4:** Re-confirm the three Part A defects at this head with the anchors in Tasks 2-4; record any moved line numbers.

### Task 2: Portable implicit rule source (`src/server.rs:4729` region)

**Files:** Modify `src/server.rs:4729-4749` (backfill). Test: `src/server.rs` tests near the existing shareable-rule fixtures.

**Interfaces:**
- Consumes: the rule branch reads `locations[0].path` (`server.rs:3998-4013`, `:4624-4628` — re-locate at the new head before writing the test), not the top-level `location`; the publish gate and `projected_metadata` (`worker.rs:248-252,502,544,762-771` — unchanged by #300); the RDF source triple (`rdf/dataset_canonical.rs:72-74`).
- Produces: implicit `source` is `"agent"` whenever the resolved input is absolute, unplaced or absent; the resolved project-relative spelling otherwise. Explicit caller `source` is never overwritten. Relative input on main returns verbatim with no root (`paths.rs:363-369`), so a relative spelling is only emitted when a root actually placed it.

- [ ] **Step 1: Failing tests per the AC1 matrix.** Baseline pinned to the leaked raw source.
- [ ] **Step 2:** Verify each baseline failure at the backfill.
- [ ] **Step 3:** Implement the two-branch backfill; never copy a stored spelling the resolver did not place.
- [ ] **Step 4:** Full gate. Commit: `fix(memory): an implicit rule source is portable, never the raw location`.

### Task 3: Char-safe evidence-id slice (`src/store/ladybug.rs:10771`)

**Files:** Modify `src/store/ladybug.rs:10796-10816` (the `&h[..h.len().min(12)]` slice). Test: `src/store/ladybug.rs` tests near the evidence-id fixtures (`:10758-10781` shows the inputs: resolved declaration, file, `file_sha256`/`span_sha256` strings).

**Interfaces:**
- Produces: a boundary-safe 12-byte-cap truncation; ordinary non-ASCII content keeps its ASCII hex fallback hash unchanged (only a supplied non-ASCII hash string can hit the slice).

- [ ] **Step 1: Failing test per AC2:** exact persisted evidence ID with a multibyte char across byte 12; long ASCII hash unchanged.
- [ ] **Step 2:** Verify the baseline panic.
- [ ] **Step 3:** Implement the boundary-safe truncation.
- [ ] **Step 4:** Full gate. Commit: `fix(store): the evidence-id hash slice never splits a character`.

### Task 4: One scan set per logical file (`src/store/ladybug.rs:13646`, inherent method)

**Files:** Modify `src/store/ladybug.rs` — the three physical query sites (`:12815` exact-file, `:12837` directory, `:12881-12900` metadata fallback) and the multi-form dispatch (`:13637-13646`); the method `memories_for_file_forms` (`:13646-13715`) returns `MultiFormMemoryRetrieval` (`:13336-13358`) with per-form cap 1,024 (const `:13358`, truncate `:13633-13634`), OR-ed truncation, retained successes, first-error-if-all-fail, then global rank/dedup/cap. Test: beside `memories_for_file_forms_preserves_global_best_hit_across_form_cutoff_boundary` (`:16118`) and `..._enriches_bridges_once_per_logical_file` (`:16190`), both passing at the base.

**Interfaces:**
- Consumes: the per-form list per logical file that the server already passes; existing `cfg(test)` counters count different work (fallback fan-out `:13579-13587`, distinct-form dispatch `:13663-13672`, enrichment `:13345-13348`) — new counters go at the physical query sites.
- Produces: byte-identical observable behavior per AC4 with one physical scan set per logical file per AC3; shared-query failure falls back to per-form queries.

- [ ] **Step 1: Failing counter tests per AC3** (baseline 2,2,2 → final 1,1,1; conditional variants; duplicate spellings).
- [ ] **Step 2: Differential regression per AC4** — capture the per-form baseline results first, then assert the batched method reproduces them exactly, including the 1,024-cap cases and the failure matrix.
- [ ] **Step 3:** Batch the three query families (escape per form, `m.location IN (...)`), keep the return contract, add the batch-failure fallback.
- [ ] **Step 4:** Both existing tests unchanged and green; full gate; no timing assertion.
- [ ] **Step 5:** Commit: `perf(memories_for_file_forms): one physical scan set per logical file`.

### Task 5: Root-prefix verification only (no implementation)

**Files:** none modified; ledger records.

- [ ] **Step 1:** At the base SHA, run `cargo test --locked --bin neurostrata-mcp paths::tests::prefix_equal_to_configured_or_checkout_root_resolves_to_root_forms -- --test-threads=1 --exact` (plus any sibling the pinned CI log names). Expected: pass (the test asserts `.` and root forms for project, registry and marker roots, foreign-checkout veto, and File-scope rejection — `paths.rs:1595-1648`).
- [ ] **Step 2:** Record SHA, command and output in the ledger. The defect does not reproduce on main; the stack variant is the author's, noted in the #279 comment with main's passing test as the reference behavior.

### Task 6: Registration, clean-checkout verification, and audit (owns the eval suite)

**Files:** Modify `eval/registry.json` (register `paths-main-hygiene`). Ledger updates.

- [ ] **Step 1:** Register the suite with the exact fully qualified test names from Tasks 2-4 and the Task 5 test, `thresholds: {ignored: 0}`, commands in the per-task filter form.
- [ ] **Step 2:** From a clean checkout of the final commit, run `scripts/remediation-eval.sh paths-main-hygiene`; require exit 0 and the recorded expected-test list to match.
- [ ] **Step 3:** Run the full local gate; run `./target/debug/neurostrata-mcp task validate` and, before any user-approved push, `task gate --strict` — record both.
- [ ] **Step 4:** Audit: base-to-head diff contains only the agreed files; ledger has baseline-red and final-green for every new test; zero public writes without entries. Upstream ruleset checks run on the PR after user-approved push and PR open.

### Task 7: Author review-comment package (Part B; five local files)

**Files:** Create the four `/tmp/ns-panel/review-comments-*.md`; required topics per file:

- **`review-comments-279.md`:** (a) the `https://` join mangling — the probe on this PR's head showed primary `https:/example.com/a`, a joined URI form, and a rooted alternate even for a qualified graph ID like `ns::id`: the predicate must cover scheme-bearing strings (`://`, `::`) AND treat qualified graph IDs distinctly, while genuine basenames (`Cargo.toml`) stay file-like; (b) main's relative branch already returns such strings byte-identical (`8cc2dc2f:src/paths.rs:363-369`) with its root-prefix test passing — adopt main's behavior; (c) #280's write normalization (`dc94eadf:src/server.rs:2844-2857`) matters when promising byte-preserving writes; (d) the root-prefix-at-root and relative-`related`-seed items from the killed v1: handed off here, not dropped silently.
- **`review-comments-280.md`:** (a) the governed-slash fix lacks an end-to-end `add_memory` → stub → `AncestorDirectory` test (a fresh handler-to-ancestor assertion checking rule ID and typed match); (b) fresh-write scope: an already-mistyped legacy stub survives a corrected declaration (`ladybug.rs:7145,12500` resolve an old ID and type only on create); (c) `metadata.absolute_path` exposure is written at **two** constructors — live capture (`mesh_outbox.rs:205-216`) and replay capture (`:677-690`) — one shared scrub must cover both; check a persisted row after a live event and after replay of a pre-stamped payload; prevention of new writes is distinct from cleanup of already-persisted entries; the export reader (`ladybug.rs:12246` at main) stays local and unchanged; (d) the write-root question goes to the maintainer, not this PR.
- **`review-comments-281.md`:** the migration cannot land as a generic `upsert` loop (`paths_migration.rs:63-94`; `ladybug.rs:10812` restamps matched edges; `traits.rs:448,516-533` has no rewrite API): acceptance outcomes to add — historical/receipt row selection (receipts keyed by `memory_type=="action_receipt"`, `rdf/action.rs:2455`), edge-provenance preservation, crash recovery, and the real-store dry run the issue's own Order and this PR's body both mark pending; a preimage journal is one **proposal**, not a requirement the issue imposes.
- **`review-comments-282.md`:** (a) the flag never clears and at least five write routes bypass the resolver (CLI add `main.rs:8800`, edit `server.rs:11251`, vault `vault_sync.rs:637`, mesh conflict annotation `synaptic_mesh/import.rs:1483`, inherited receipt location `rdf/action.rs:3587` — the vault path writes canonical locations on changed note rows but does not repoint the registry root); either a per-row derived flag or the issue's step-4 fallback with a counter — reversing this PR's documented design is the author's/maintainer's call, presented as a question; (b) the one-spelling defects live in the AST wrapper (`server.rs:3095-3115` at `10debfdb`: first populated AST wins), not the governing-memory wrapper: suggest disjoint-symbol alias-union tests and retained truncation across later hits, green in **both** flag modes; (c) the closure wording for #270-#272 (closed by #283) is the author's to edit; no reviewer edits PR bodies.

- [ ] **Step 1:** Re-verify every anchor against each PR's head at drafting time (force-push history: 5/9/11/14 events); pin citations to repo/SHA/file/line.
- [ ] **Step 2:** Write the four files with the topics above; hash each draft; record in the ledger.
- [ ] **Step 3:** On user approval per PR (ledger entry with draft SHA and head SHA re-verified immediately before posting): `gh pr review <n> --comment --repo Cognilogical/NeuroStrata --body-file <file>`; record URLs.

### Task 8: Maintainer question on issue #275 (Part B; one local file)

**Files:** Create `/tmp/ns-panel/maintainer-question-275.md`.

**Interfaces:**
- Consumes: the issue's actual three open questions and the corrected evidence below.
- Produces: one short comment, posted only on recorded approval.

- [ ] **Step 1:** Draft with: (a) **the issue's own root question** ("should the registered root win?") — evidence: the stack answers "only when nested inside the caller's" (`dca6dd04:src/paths.rs:514-522`), main's #283 answers "caller wins, deeper registered root becomes an alternate" (`8cc2dc2f:src/paths.rs:377-400`); whichever root wins needs its own root-move rule first, because `append_log` can replace a different existing registered root on a successful upsert (`buffer.rs:318-324,526-529`), and so do snapshots (`server.rs:5398`) and ingest (`parser/ingest.rs:2552`); (b) the **new** post-#283 question: given the merged multi-form resolver and #283's reported benchmark, does #275's write half (#280) proceed, and do the migration and flag layers (#281/#282) remain wanted at all — the author's stack comment ties #282 to the issue's outside-paths question (2); (c) the late absolute-write case (Review Focus 5) as part of (b), with the five bypass routes.
- [ ] **Step 2:** Hash the draft; ledger; on approval post once; record the URL.

---

## Bead disposition (the excalibur-quest record, updated by direct local edit of `/tmp/ns-head/.excalibur_quest/quest_db.json`; never `sync`, never `close`)

| Bead | v1 finding | Disposition |
|---|---|---|
| 1 | governed slash | HANDED OFF to `review-comments-280.md` (end-to-end test gap, fresh-write scope) |
| 2 | root authority | HANDED OFF to `maintainer-question-275.md` (the issue's own question 1) |
| 3 | flag semantics | HANDED OFF to `review-comments-282.md` + maintainer question (b) |
| 4 | bridge/closures | WITHDRAWN as code work — #270-#272 closed by #283; regression tests HANDED OFF to `review-comments-282.md` |
| 5 | migration safety | HANDED OFF to `review-comments-281.md` (dry-run precondition; journal labeled a proposal) |
| 6 | `absolute_path` | HANDED OFF to `review-comments-280.md` (both capture paths, one shared scrub) |
| 7 | rule `source` leak | VERIFIED ON MAIN (`server.rs:4729` at `cda06dce`) → Part A Task 2 |
| 8 | `https://` mangling | HANDED OFF to `review-comments-279.md` (scheme + qualified-ID distinction) |
| 9 | per-form scans | VERIFIED ON MAIN as partially shipped by #283 (`memories_for_file_forms`); remainder → Part A Task 4 |
| 10 | smaller items | SPLIT: hash slice → Part A Task 3 (on main); prefix-at-root → Part A Task 5 (verified passing on main); relative `related` seed → HANDED OFF to `review-comments-279.md` |

## Sequencing

Task 1 → Tasks 2-5 (Part A; each commits with its own red/green ledger entries) → Task 6 (registration + audit) → user-approved fork push and upstream PR → Tasks 7-8 (Part B drafting may start early; posting waits for per-item approval). The stack's rebase onto `8cc2dc2f` is the author's move, tracked in the review comments; our branch never touches their fork.
