# Anthropic-Memory Parity & Beyond — Implementation Plan (v1, post-wringer)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Fresh implementer + fresh reviewer per task; whole-branch independent review at every milestone gate.
>
> **Version:** v2 — v1 was the clean rewrite after the planning-wringer panel (2026-11-07): Feasibility FEASIBLE-WITH-CHANGES, Spec/Test SOUND-WITH-CHANGES, Risk ACCEPTABLE-WITH-CHANGES, Devil's Advocate KILL-PART. The independent supreme judge ruled v1 **APPROVED-WITH-CHANGES**; its five required changes (RC1–RC5) are applied in this version and recorded at the end of the Panel record. Draft v0 and v1 are superseded.

**Goal:** Close the measured gaps between NeuroStrata and Anthropic's published agent-memory model — offline consolidation, general knowledge-wiki representation, retrieval fusion, curated injection — inside the NeuroStrata fork, under NeuroStrata's own guarantees (supersede-only lifecycle, provenance on derived artifacts, fail-closed security, local-first, any-LLM), and only where Phase 0's published evidence shows the gap is real.

**Architecture:** Evidence first (a benchmark harness with git-committed, pre-registered thresholds — before any baseline is run). Consolidation ships as an **external batch tool** (the `neurostrata-mcp consolidate` CLI), never as in-daemon machinery in this plan: it runs against a quiesced store or a store copy under the environment's sanctioned write route (graceful shutdown → write → leave daemon down), orchestrating a five-stage pipeline — Collect → Verify → Judge → Transform → Commit — whose only write operations are `supersede`/`archive` with evidence provenance, gated by a confidence policy table and a review queue. Representations are lease-guarded wiki pages in a dedicated namespace; retrieval gains an additive fused mode; the boot snapshot gains a token-budgeted curator. There is **no enterprise server-mode phase in this plan** (see §0).

**Tech Stack:** Rust (existing daemon/store code, rmcp MCP surface), Laya ONNX duplicate judge (existing trait — duplicate-only, see §0 correction), Tantivy + in-house BM25 (upstream tree; see Milestone 0), Python 3 for the benchmark harness (tooling only), fork GitHub Actions CI as the Rust test venue.

**Spec:** The three analysis documents (2026-11-07), in `~/workspace/your_files/neurostrata-vs-anthropic-memory/`: `01-gap-analysis.html` (gap register G1–G6), `02-feature-catalog-roadmap.html` (target catalog + phase exits), `03-technical-design-architecture.html` (design). Where this plan deviates from the specs, the deviation is recorded inline (panel adjudications, §0).

## 0. Panel adjudication — what changed from v0, and spec corrections

**Kills accepted (Devil's Advocate, corroborated by Risk):**
- **Phase 4 (enterprise server mode) is REMOVED from this plan.** G6 was an evidence gap, not a build order; server mode becomes a separate future election only if a named deployment requirement enters the record. It is replaced inside Phase 0 by an *honest scale note*: measured single-node capacity numbers, published either way. Acceptance mapping updated (Criterion 1).
- **The instruction-revision meta-loop (old Task 4.3) is KILLED.** Goal profiles remain, human-authored only. A system whose brand is enforcement does not rewrite its own governing instructions, even through a queue.
- **In-daemon consolidation placement is KILLED.** The engine is an external batch tool in this plan. Daemon integration is a separate future election, gated on the store's unflushed-freshness-flag debt being cleared first. (Consequence: no daemon idle hook, no daemon.rs surgery; scheduling is cron/manual, which also matches Anthropic's own between-sessions model.)

**Reshapes accepted:**
- **Milestone 0 is now census + sync resolution**, with a hard gate (Risk R3 / DA-2): Phase 1 branches only when (a) the upstream sync to the named target has landed on `develop` and census + baselines are re-run on the new base, or (b) Pratik explicitly elects to build on `6ce7021` with the rebase cost priced in the M0 report. "Disposition recorded" alone does not open the gate.
- **Phase 0 is anti-gaming hardened** (DA-5 / Risk R7 / Spec P1-3): fixtures, thresholds, margins, and A/B parameters are committed to git *before* baseline runs; a held-out question set is sealed; corpus/question authorship is assigned to agents who will not implement Phases 1–2.
- **Phase 0 backend is provenance-pinned** (Feasibility F4 / Risk R2): the measured backend is a CI-built binary artifact; every run record carries its commit SHA; the harness refuses a mismatched backend and refuses the production store/port (Risk R1, mechanically tested).

**Spec corrections recorded (the analysis docs overstate two things):**
- Develop's version is **Cargo 1.14.3**, not "v1.13.0-era" (v0 plan error).
- **Laya is a duplicate judge only.** The judgment trait (`src/judgment/mod.rs`) exposes `judge_duplicates` and nothing else; Doc 02's "duplicate/contradiction judge" catalog line and FEATURES.md's wording overstate it. Contradiction detection is new work in this plan (Task 2.7), not an existing primitive. Precision (judge's RC4): the trait *surface* is the same on upstream v1.16.1, but the provider internals differ — develop runs `MockJudgmentProvider` in the Laya slot (with the fork's honest-Unavailable semantics), while v1.16.1 runs `HeuristicJudgmentProvider` plus shadow-verdict machinery develop lacks — so the M0 sync changes the judging behavior behind Task 2.6's sweep, and Task 0.1's census records the active provider per mode.

## Global Constraints

- **Venue:** all code lands on the FORK `pratik-saptarshi/NeuroStrata`, base branch `develop`. Nothing is filed, pushed, or PR'd against upstream `Cognilogical/NeuroStrata` without Pratik's explicit per-action consent.
- **Mechanical store protection (not prose):** all tooling built by this plan takes its store path / daemon endpoint from a mandatory env var and **refuses the production store path and production port at startup**, with a test proving refusal. Implementation subagents never invoke `neurostrata-mcp shutdown` and never use the CLI write path against the live store; the task reviewer checks the command log — a violation is a task failure. A pre-push guard (M0 deliverable) rejects pushes whose remote is upstream.
- **Daemon-contact rule:** the live production store and the running 1.9.0-track daemon are never touched by implementation work. All store work uses throwaway stores or the named soak copy (§Phase 1 gate).
- **No Rust toolchain on the Muse VM** (`cargo` absent, verified 2026-11-07). Rust RED→GREEN runs through fork CI: the *test (ubuntu)* check is the iteration signal; the 3-OS release build matrix is gate-only. Whether to install a local toolchain is an explicit election put to Pratik at the Milestone 1 gate — not assumed, not buried (Risk R8).
- **Non-destructive writes only:** consolidation commits are `supersede`/`archive` operations with provenance. No task may add a destructive delete path anywhere.
- **Security inheritance:** every new write path passes the existing fail-closed secret scanner and caller-path authority checks. Consolidation writes are additionally bound by the commit policy table (Task 2.9).
- **Merges are Pratik's per-PR word.** PRs may be opened per feature-pr-pipeline; nothing auto-merges. **WIP limit: no new task branches while more than 3 program PRs await merge.**
- **Planning documents ship in the PR** they govern, under `docs/plans/` in the fork repo. Phase reports are public by that rule; public placement of Phase 0 results is confirmed as Pratik's election at the Milestone 1 gate (Risk R7b).
- **Displacement statement:** this program competes for Pratik's merge/decision bandwidth with: Wave 3 PRs #19–#22 (approved, awaiting merge words), T15a/T16 filings, T19 Gate 2, upstream #54/#55 merge-order work, backlog elections B3/B4/B7/B8/B10/B13, and the BGE small-model plan. Electing a phase here knowingly defers bandwidth there; per-phase PR counts are stated at each milestone so the load is visible before the election.

## Review Focus

1. A consolidation run interrupted mid-commit must leave the store consistent and resumable — never half-superseded chains, never duplicated commits on resume.
2. A memory whose provenance evidence cannot be retrieved is **Unverifiable** — never auto-corrected. "Pruned per retention policy" is a legitimate terminal state for evidence, recorded as such.
3. Two writers (engine + live session, or two engine runs) must never edit the same wiki page concurrently; enforcement lives at the server/store write path, not in caller cooperation.
4. A wiki page that merely restates its source fails the abstraction check (token-overlap metric, Task 3.3), not just the size budget.
5. No memory is promoted into the injected snapshot without a persisted verification record; demotion of guard/safety-class memories is review-queue-only at any confidence.
6. Secret-bearing content is rejected on every new write path (commit stage AND wiki builder), exactly as on session writes.
7. No prefix of Phase 1's merged tasks yields a writable engine: Verify/Judge/Transform produce data only; the first task that can write is Task 2.9, and the tool stays trigger-less until Task 2.11.

---

## Milestone 0 — Baseline census + sync resolution (gate for everything)

### Task 0.1: Feature-baseline census of fork `develop`
**Files:** Create: `docs/plans/2026-11-07-memory-parity/baseline-census.md` (in the execution PR).
**Interfaces:** Produces: a primitive-by-primitive table — every primitive later phases rely on, marked PRESENT / ABSENT / DIVERGED on `develop` with file:line evidence. The checklist must include, from panel findings: the judgment trait's exact surface **and the active dedup provider identity per mode** (judge's RC4 — the sync changes it); Thalamic Bus event classes and whether *every* memory mutation emits a sequenced event (Feasibility F2 / Spec P3-4); episodic-buffer retention/pruning behavior; search module shape (one vector path in `src/store/ladybug.rs` on develop vs three surfaces upstream); `include_ast_edges`; snapshot contents; `rebac.rs`/`did.rs` presence; supersede/archive ops (known PRESENT).
- [ ] Enumerate the checklist from Docs 01–03 + the panel findings above.
- [ ] Verify each against the `develop` tree (`git show` / `grep`), recording file:line.
**Acceptance:** every relied-upon primitive has an evidenced verdict; known results pre-registered in this task so the census confirms rather than discovers them: judgment = duplicate-only; `src/fts`, `src/lexical.rs`, `src/rdf`, `src/community.rs` ABSENT on develop; divergence ≈772 commits / 154 src files **at panel time — the census re-derives both numbers at run time, since upstream moves (the judge measured 774 two hours later)**.

### Task 0.2: Sync-resolution plan + guards
**Files:** Create: `docs/plans/2026-11-07-memory-parity/sync-resolution.md`; Create: pre-push upstream-remote guard (script + install step in the same PR).
**Interfaces:** Produces: the named sync target and path. Target: **upstream commit `8fdae4b`** — PR #57, child of `cd66291` (PR #56); the target therefore contains v1.16.1 + PRs #56 and #57. (Judge's RC1: v1 named `cd66291` while describing it as containing #57; `cd66291` IS #56 and does not contain #57.) NOT PR #23, which reaches only v1.15.0 and lacks part of the analyzed set (Feasibility F5). The target is SHA-pinned; if upstream has advanced past `8fdae4b` when the sync executes, the delta is re-censused under Task 0.1 rather than silently absorbed.
- [ ] Record the path: disposition of PR #23 (its CI red is upstream issue #42, the gitignored `plugins/opencode/dist/index.js` build defect; the fork's own #48 pattern is the proven fix shape), the T15a/T16 sequencing decision, and the forward-merge semantics that must be preserved (Laya honest Unavailable/errors semantics per the v1.14.3 merge).
- [ ] Price the fallback: if the sync cannot land within the window Pratik sets, the partial-sync option (cherry-pick only the primitives Phase 1 needs) with its estimated rebase cost — or the build-on-`6ce7021` election with the same pricing.
- [ ] Install and test the upstream-push guard: a push attempt with upstream as remote is rejected; a fork push passes.
**Acceptance:** sync-resolution names target, path, fallback, and price. **M0 gate (hard):** Phase 1 may not branch until (a) sync landed + census re-run on the new base, or (b) Pratik's recorded election to build on `6ce7021` / partial-sync with the priced rebase accepted. Sync *execution* itself proceeds only under Pratik's separate authorizations (filings and merges are his words, as always).

### Task 0.3: Test-venue runbook (measured)
**Files:** Create: `docs/plans/2026-11-07-memory-parity/test-venue.md`.
- [ ] Run one real pilot Rust task end-to-end (a trivial test-only change) and record measured CI round-trip wall-clock for: branch → PR → *test (ubuntu)* result.
- [ ] Document: the iteration loop, the gate loop (full matrix), per-phase CI-run budget estimates derived from the pilot, and the ghg budget-gate behavior (exit 75 = stop and report).
- [ ] Acceptance check performed, not asserted: a fresh subagent executes one full RED→GREEN cycle using only the runbook.
**Acceptance:** runbook carries measured numbers; the fresh-agent check is logged.

---

## Phase 0 — Evidence: public memory benchmark suite (Milestone 1)

**Purpose:** Build the yardstick before building the features (Gap G5) — and test whether the gaps are material (DA-1: G1's premise must be *shown*, not assumed). Pure tooling; no product-code changes. **Authorship separation:** fixture corpus, questions, and thresholds are authored by agents who will not implement Phases 1–2.

### Task 1.0: Backend provenance + store protection
**Files:** Create: `tools/membench/backends.py` (partial — provenance + guard), `tools/membench/tests/test_backend_guard.py`.
**Interfaces:** Produces: `resolve_backend() -> (binary_path, commit_sha)` reading a CI artifact manifest; every later run record embeds the SHA.
- [ ] Failing tests: harness pointed at the production store path or production port → refuses to start; backend SHA ≠ target tree SHA → refuses to run.
- [ ] Implement: CI artifact download/pin step for the `develop` binary (fork CI build job artifact), SHA recording, env-var endpoint resolution with refusal list.
**Acceptance:** both refusals proven by tests; a run record shows the measured binary's SHA.

### Task 1.1: Harness core + backend interfaces
**Files:** Create: `tools/membench/runner.py`, `tools/membench/report.py`; Test: `tools/membench/tests/test_runner.py`.
**Interfaces:** Produces: `MemoryBackend` (`add`, `search`, `supersede`, `archive`, `snapshot`, `ingest`) and `Corrector` (the interface the Phase 1 engine will implement: `run_pass(store) -> PassReport`) — both defined here so suites consume, never invent (Spec P2-8). Backends: NeuroStrata adapter (via Task 1.0's binary) and the file-folder baseline (plain Markdown files, the Anthropic shape).
- [ ] Failing test: runner executes a scripted sequence against a stub backend and emits one run record per item, SHA-stamped.
- [ ] Implement runner, both real backends, report generator.
**Acceptance:** both backends complete a smoke suite; report renders per-suite scores.

### Task 1.2: Learning-loop suite (+ coding arm for the Done-Funnel A/B)
**Files:** Create: `tools/membench/suites/learning_loop.py`, `tools/membench/suites/coding_tasks.py`; Tests: `tools/membench/tests/test_learning_loop.py`.
- [ ] Discrimination oracle first: a scripted backend that records corrections scores measurably higher in later rounds than one that doesn't; a no-memory control scores at the stateless baseline.
- [ ] Learning-loop suite: sequential questions, correct answer revealed each round (CL-Bench shape); score = later-round accuracy vs stateless.
- [ ] Coding arm: a small suite of repository-shaped tasks (the domain the Done-Funnel actually operates in) with the same reveal-and-retry shape.
**Acceptance:** both suites discriminate per the oracle test.

### Task 1.3: Seeded-error correction suite
**Files:** Create: `tools/membench/suites/seeded_error.py`; Test: `tools/membench/tests/test_seeded_error.py`.
**Interfaces:** Consumes: `Corrector`. Produces: correction precision/recall + a **materiality metric**: fraction of planted-wrong memories that survive write-time judging AND are retrieved/acted on in probe questions.
- [ ] Seed a store: N memories, K planted-wrong with known ground truth and planted transcript evidence.
- [ ] Oracle tests: perfect corrector → precision 1.0 / recall 1.0; no-op corrector → recall 0.
- [ ] Materiality probe: after seeding, run probe questions; record whether wrong memories surface as answers.
**Acceptance:** suite discriminates; materiality metric reported separately from precision/recall.

### Task 1.4: Corpus-QA suite + fixtures
**Files:** Create: `tools/membench/suites/corpus_qa.py`, fixtures under `tools/membench/fixtures/corpus/` (public-domain / synthetic only), a **churn fixture** (planted stale rules + anchor churn) and a **contract fixture** (document set + graded fact list) for Phase 3's gates; Test: `tools/membench/tests/test_corpus_qa.py`.
- [ ] Failing tests: per-class known-answer checks; a by-construction separation check — every cross-document question's answer terms are absent from every single fixture document (Spec P2-2).
- [ ] Two question classes, scored separately forever: cross-document (where Anthropic's wiki won) and single-document (where it gained nothing).
- [ ] A held-out question set: committed as a sealed file (hash recorded in the registration commit, Task 1.5); opened only at milestone-gate runs.
**Acceptance:** suite runs end-to-end search-only; class separation proven by construction test.

### Task 1.5: Pre-registration commit (BEFORE any baseline run)
**Files:** Create: `tools/membench/registration.md` + the sealed held-out set from Task 1.4 — committed to the fork in a dedicated PR that lands **before** Task 1.6 runs anything.
**Contents (all numbers fixed here, not after results):** M2 correction precision/recall floors; **the seeded-error materiality floor that Task 1.6's premise check turns on** (judge's RC2 — without it the premise check is prose); M3 cross-document margin + single-document noise band; **the wiki size-budget fraction and the abstraction token-overlap threshold** consumed by Task 3.3 and the M3 gate (RC2); **the churn drift-detection rate** consumed by the M4 gate (RC2); M2 soak queue-burden ceiling (queue depth and reviewer-minutes per pass) and approval-rate sanity bound; Done-Funnel A/B parameters (model ID, question-set hashes, scoring rubric) and its **decision rule**: e.g. if enforcement delta < −0.05 on both arms, the Milestone 1 report must present the owner election "funnel advisory vs mandatory" as a decision item for Pratik with the data attached — the report may not bury it; single-node scale-note method (concurrency steps, latency percentiles to publish). **Definition (judge's observation):** "baseline run records" = all suite runs from Task 1.6 onward; the smoke and end-to-end plumbing runs in Tasks 1.1/1.4 are not baseline records, and the predates check applies to Task 1.6's runs.
- [ ] Registration PR merged; its commit hash is cited by every later gate.
**Acceptance:** the commit exists and predates every baseline run record (verifiable by SHA timestamps in run records).

### Task 1.6: Baseline report + premise check + scale note
**Files:** Create: `docs/plans/2026-11-07-memory-parity/phase-0-baseline-report.md` (via execution PR).
- [ ] Run all suites: fork NeuroStrata (SHA-pinned binary) vs file-folder baseline; publish all numbers, including negative ones.
- [ ] Done-Funnel A/B on both arms (learning-loop + coding), parameters exactly as registered.
- [ ] Honest scale note: measured single-node capacity (concurrent sessions, query latency percentiles, store sizes) — published whichever way it lands; this is G6's closure in this plan.
- [ ] **Premise check section (required):** in plain terms, which later phases the evidence still justifies — including, if the seeded-error materiality rate is negligible, the statement that Phase 1's premise failed and it should not be elected. The report's author does not get to soften this section; the registration commit fixed what "material" means.
**Milestone 1 gate:** registration commit exists and predates runs; baseline report merged with premise check intact; the local-Rust-toolchain election and the public-placement confirmation are put to Pratik as explicit questions alongside the Phase 1 election. Phase 1 starts only on Pratik's word, AND only when the M0 hard gate (§Task 0.2) is satisfied.

---

## Phase 1 — Consolidation tool: correction & hygiene (Milestone 2)

**Purpose:** Close G1's core — an offline pass that corrects memory against evidence — as an **external batch tool** (`neurostrata-mcp consolidate`), run against a quiesced store or store copy. Depends on: M0 hard gate, Milestone 1 (oracle + registered thresholds), and the premise check not having failed. Task order matters: Tasks 2.1–2.8 produce data only and cannot write (Review Focus #7); the first writable task is 2.9.

### Task 2.1: Consolidation event class + mutation coverage
**Files:** Modify: `src/events/outbox.rs`, `src/events/bus.rs`, `src/events/subscribers/` (new `consolidation.rs` subscriber); Tests: inline.
**Interfaces:** Produces: a `Consolidation` EventClass; every memory mutation pulse (create / supersede / archive), regardless of shareability or type, is durability-qualified and receives an outbox `seq`. (Today only Audit/Mesh classes exist and Mesh captures only shareable rule/fact/procedure — Feasibility F2.)
- [ ] Failing tests: a task-type memory mutation and a private (non-shareable) memory mutation each receive a durable seq with the Consolidation subscriber registered; without the subscriber, behavior is unchanged.
- [ ] Implement the class + subscriber + `interested_in` coverage for all three mutation pulses.
**Acceptance:** mutation coverage is total, proven by tests across types and eligibility.

### Task 2.2: Watermark + pending range
**Files:** Create: `src/consolidation/mod.rs`, `src/consolidation/watermark.rs`; Tests: inline.
**Interfaces:** Produces: `ConsolidationWatermark { last_event_seq, last_pass_id, completed_at }`, persisted in the store; `pending_range(store) -> Vec<MutationRef>` — exactly the mutations since the watermark. (No `log_ranges` in the contract; log access is via citations, Task 2.3.)
- [ ] Failing tests: scripted mutations → `pending_range` returns exactly them; after `advance()`, returns none; a run killed before `advance()` re-presents the same range.
- [ ] Implement watermark persistence + range query.
**Acceptance:** watermark semantics proven; the tool binary does not yet exist (library only).

### Task 2.3: Evidence citations (new substrate)
**Files:** Modify: memory write path (`src/server.rs` add/task-complete handlers) + `src/traits.rs` metadata schema; Create: `src/consolidation/citation.rs`; Tests: inline + fixture logs.
**Interfaces:** Produces: a structured citation format on extracted memories — `{ session_file, entry_marker, captured_at }` — stamped at extraction time; a resolver `resolve(citation) -> EvidenceRange | Pruned | Missing` that pins cited files against Episodic Buffer retention pruning (cited files exempt from `prune_rolled`, or citation ranges copied into the store at stamp time — implementer's choice, test decides behavior either way). (Today `metadata.source` is unvalidated free text — Feasibility F3.)
- [ ] Failing tests: extraction stamps a citation; resolver returns the range; a citation into a pruned file returns `Pruned` (a terminal, non-error state), never a panic or a false range.
- [ ] Implement schema + stamping + resolver + pruning interaction.
**Acceptance:** citations exist on newly extracted memories; legacy memories without citations resolve as `Missing` → Unverifiable downstream.

### Task 2.4: Verify stage — anchor re-resolution
**Files:** Create: `src/consolidation/verify.rs`; Tests: inline + fixture store.
**Interfaces:** Produces: `AnchorVerdict::Resolves | Moved { new_location } | Vanished | Unverifiable` per code-anchored memory. **Moved rule (specified, Feasibility F6):** same namespace + same `span_sha256` under a different name/path ⇒ Moved (requires adding a by-span-hash symbol lookup to this task's scope); no span-hash match ⇒ Vanished if the file is gone, else Unverifiable.
- [ ] Failing tests: intact anchor → Resolves; renamed symbol with identical span hash → Moved with the correct new location; deleted file → Vanished; anchor without provenance → Unverifiable.
- [ ] Implement by-span-hash lookup + verdict computation. Read-only.
**Acceptance:** all four verdicts discriminated on fixtures.

### Task 2.5: Verify stage — transcript-grounded checking
**Files:** Create: `src/consolidation/verify_transcript.rs`; Tests: fixture logs + scripted mock judge.
**Interfaces:** Consumes: Task 2.3 citations; an LLM-judge adapter trait (mock in tests; the production adapter is exercised once in the soak, Task 2.11). Produces: `ClaimVerdict::Supported | Contradicted { evidence } | Unverifiable`. Citation `Pruned`/`Missing` ⇒ Unverifiable, always (Review Focus #2).
- [ ] Failing tests: supported claim → Supported; contradicted → Contradicted carrying the evidence range; missing/pruned citation → Unverifiable and never Contradicted.
- [ ] Implement citation retrieval + judge call behind the trait. Read-only.
**Acceptance:** the Unverifiable paths are proven; no production LLM is called in unit tests.

### Task 2.6: Judge stage — dedupe sweep (existing trait)
**Files:** Create: `src/consolidation/judge.rs`; Tests: inline.
**Interfaces:** Consumes: Laya `judge_duplicates` (duplicate-only). Candidate generation is **pending-range memories vs store-wide nearest neighbors** — never range-internal only (Spec P2-4). Produces: duplicate clusters with Laya scores.
- [ ] Failing tests: planted duplicate pair, one member outside the pending range → cluster found; unrelated pair → no cluster.
- [ ] Implement sweep. Read-only.
**Acceptance:** the out-of-range duplicate — the common case — is caught by test.

### Task 2.7: Contradiction detection (new capability)
**Files:** Modify: `src/judgment/mod.rs` (trait extension) OR Create: `src/consolidation/contradiction.rs` on the Task 2.5 judge-adapter seam — implementer picks per the trait's shape, reviewer verifies the choice against F1's evidence; Tests: inline with scripted judge.
**Interfaces:** Produces: contradiction candidate pairs with evidence (the two memory bodies + verdict provenance). This task exists because no contradiction judge exists in any base (Feasibility F1).
- [ ] Failing tests: planted contradictory pair (same subject, opposed claims) → candidate; planted complementary pair → no candidate; judge disagreement/unavailable → no candidate (fail-closed).
- [ ] Implement behind the adapter trait.
**Acceptance:** discriminating tests pass; the capability is honestly new code, not a wrapper claim.

### Task 2.8: Transform stage — successor drafting
**Files:** Create: `src/consolidation/transform.rs`; Tests: inline with mock drafting adapter.
**Interfaces:** Consumes: `AnchorVerdict` + `ClaimVerdict` + duplicate clusters + contradiction candidates + gap candidates (Task 2.12). Produces: `CandidateChange { id, kind: Correct | Merge | Archive | Create, targets, successor_content: Option<String>, evidence, confidence }` — `id` is the idempotency key; `successor_content` is what Commit writes (Spec P1-1). `Create` (judge's RC3) covers gap-filling's new memories, which have no supersede target; it is **queue-only by schema** — the policy table (Task 2.9) has no auto-commit row for it.
- [ ] Failing tests: `Contradicted` verdict → `Correct` candidate whose successor content differs from the original and cites the contradicting evidence; duplicate cluster → `Merge` candidate with merged content; gap candidate → `Create` candidate with drafted content and log evidence; `Supported`/`Resolves` → no candidate.
- [ ] Implement drafting via the judge/draft adapter trait (mock in tests).
**Acceptance:** every verdict type has a defined Transform fate; drafts are data, nothing written.

### Task 2.9: Commit policy + commit engine + rollback (FIRST writable task)
**Files:** Create: `src/consolidation/policy.rs`, `src/consolidation/commit.rs`; Tests: inline.
**Interfaces:** Produces: the **commit policy table** — (kind × confidence band × evidence class) → auto-commit | review-queue — with these hard rules baked in, not configured away: (a) auto-commit requires a deterministic signal (dead anchor per 2.4, Laya duplicate above the registered threshold) **or** two independent judge passes (different model or prompt) agreeing; a single transcript-verdict's self-confidence never reaches auto-commit alone (Risk R4b); (b) guard/safety-class memory changes are queue-only at any confidence; (c) every commit is a `supersede`/`archive` carrying provenance to its evidence bundle, and stamps the successor's verification record (`verified_at`, verdict, evidence pointer — Spec P2-5); (d) commits are grouped by `pass_id` and **bulk-rollback-by-pass** reverses a pass's supersede chains (Risk R9a); (e) `Create` candidates (Task 2.8) are queue-only; on approval they commit through the standard memory write path with the candidate's provenance attached — never a side door (judge's RC3).
- [ ] Failing tests: deterministic dead-anchor correction auto-commits as a supersede, original never mutated in place; single-judge interpretive rewrite → queue, not committed; secret-bearing successor content → rejected by the existing scanner (Review Focus #6); rollback-by-pass restores the pre-pass heads; kill mid-commit + resume → no duplicates, no half-superseded chains (Review Focus #1).
- [ ] Implement policy table + commit + rollback.
**Acceptance:** the provenance audit test walks every committed op to retrievable-or-`Pruned` evidence.

### Task 2.10: Review queue + tool surface
**Files:** Create: `src/consolidation/queue.rs`; Modify: `src/main.rs` (the `consolidate` subcommand: `run`, `status`, `review list|approve|reject`, `rollback <pass_id>`); Tests: inline + CLI integration test on a fixture store.
- [ ] Failing tests: queued candidate approved → commits through Task 2.9's path (not a side door); rejected → archived as a proposal with reason; queue persists across process restarts.
- [ ] Implement queue + CLI. (No new MCP tools in this plan; the tool is operator-run.)
**Acceptance:** the full operator loop works on a fixture store from the CLI alone.

### Task 2.11: Triggers, soak, and the production-judge exercise
**Files:** Modify: `src/main.rs` (pass orchestration: Collect→…→Commit as one `consolidate run`); Create: soak runbook in `docs/plans/2026-11-07-memory-parity/soak.md`.
- [ ] Pass orchestration with engine-wide single-run lock; change-volume no-op: a run whose pending range is empty or below a configured floor exits without LLM spend (records the descope of Doc 03's third trigger as: implemented as a floor check, not a daemon trigger).
- [ ] Bind the membench `Corrector` protocol (Task 1.1) to this CLI (a subprocess adapter in `tools/membench/backends.py`) so the M2 gate run drives the real engine through the same interface as the fixture correctors (judge's observation).
- [ ] One integration run with a real configured LLM against fixture logs (the production judge path exercised at least once — Risk R4c), in the soak context, not unit CI.
- [ ] Soak: unattended passes on the soak copy — a copy of a real project store kept at the NeuroStrata goal's `hidden_files/` (NOT `/tmp`; VM replacement must not destroy gate evidence — Risk R10), project namespaces only where feasible, with a named disposal step at gate close (deletion-rule flow or explicit retention election).
**Milestone 2 gate (all mechanical):** seeded-error suite vs the real engine: precision/recall ≥ the registered floors; **independent hand-audit**: ≥30 candidate corrections from the soak, hand-labelled by a reviewer who did not produce them — audited precision ≥ the registered floor, and auto-commit stays OFF (queue-only mode) until the audit passes; soak queue burden within the registered ceiling **and the soak's approval rate inside the registered sanity bound** (judge's observation — the bound is registered in Task 1.5 and invoked here); bulk rollback demonstrated on the soak copy. Running the tool against the *live* store is a separate owner election after this gate, not part of it.

### Task 2.12: Cross-session gap-filling
**Milestone membership (judge's RC3):** Task 2.12 is part of Phase 1 and is reported at Milestone 2, but it is **not** inside the M2 gate's mechanical criteria — the gate may close without it. It must land before the Phase 2 election, since Phase 2's wiki consumes its dangling-reference output.
**Files:** Create: `src/consolidation/gaps.rs`; Tests: fixture logs + fixture store.
**Interfaces:** Produces: gap candidates into Transform — topics with session-log evidence but no extracted memory; repeated failure mentions without a governing rule; dangling references to memories/pages never created (Doc 02 Part B). New-memory outputs become `Create` candidates (Task 2.8) and are queue-only end to end.
- [ ] Failing tests: planted uncovered topic → candidate; planted dangling reference → repair candidate; covered topic → no candidate.
- [ ] Implement as a Collect-side producer feeding Task 2.8.
**Acceptance:** gap candidates carry log evidence; they can only ever enter via the queue (no auto-commit class for gap candidates in the policy table).

---

## Phase 2 — Representations & fusion (Milestone 3)

**Depends on:** Milestone 2 (the wiki is maintained by consolidation passes; building it before the engine exists strands it). Post-sync base (M0) is a hard prerequisite for Task 3.4 — on `6ce7021` there are no three search modes to fuse (Feasibility F5); if Pratik elected the old-base path at M0, Phase 2 waits for the sync or is re-scoped by a new election.

### Task 3.1: Wiki page store + leases, enforced at the write path
**Files:** Create: `src/wiki/mod.rs`, `src/wiki/lease.rs`; Modify: the server/store write path for namespace `wiki:<project>`.
**Interfaces:** Produces: pages as memories in `wiki:<project>` with link edges; `acquire_page_lease(page_id, holder) -> Lease | Busy`. **Enforcement is at the write path** (Spec P2-6): any write to a wiki page — generic `add`/`supersede` tools included — without a held lease is rejected.
- [ ] Failing tests: second acquire while held → Busy; expired lease → acquirable; wiki-API write without lease → rejected; **generic-tool write to a wiki page without lease → rejected**; non-wiki namespaces unaffected.
- [ ] Implement lease + write-path guard.
**Acceptance:** the bypass test passes — leases are a property of the store, not of polite callers.

### Task 3.2: Wiki builder — ingest & extract
**Files:** Create: `src/wiki/build.rs` (ingest/extract half); Tests: fixture corpus from Task 1.4.
**Interfaces:** Consumes: a document corpus directory (caller-path authority applies); Produces: extracted claim sets with source pointers per claim, staged for Task 3.3.
- [ ] Failing test: extraction over the fixture corpus yields claims each carrying a resolvable source pointer.
- [ ] Failing test: a corpus document containing a planted secret → its content is rejected by the secret scanner on the write path (Review Focus #6, builder path — Spec P3-3).
- [ ] Implement ingest + extraction.
**Acceptance:** provenance-complete extraction on fixtures.

### Task 3.3: Wiki builder — page drafting, linking, budgets
**Files:** Create: `src/wiki/draft.rs`; Tests: fixture corpus.
**Interfaces:** Consumes: Task 3.2 claim sets. Produces: pages written under lease, link edges between pages, and the two mechanical gates: **size budget** (representation total ≤ the registered fraction of corpus size) and **abstraction check** (per-page token-overlap ratio against its sources ≤ the threshold fixed in the fixture; a restating page fails — Review Focus #4 / Spec P3-2).
- [ ] Failing tests: build produces pages with ≥1 link edge per page where a related page exists (link existence asserted — a zero-link build fails); a planted dangling link is detected and reported; an over-budget build is refused/flagged; a verbatim-restating page fails the abstraction check.
- [ ] Implement drafting + linking + both gates. Incremental builds coordinate via a shared progress log; one writer per page, always (Task 3.1).
**Acceptance:** fixture build passes provenance, link, budget, and abstraction checks.

### Task 3.4: Fused retrieval mode
**Files:** Modify: `src/search.rs` (post-sync shape), `src/server.rs` (add `mode: fused`); Create: `src/search/fusion.rs`.
**Interfaces:** `fuse(results_per_surface, k=60) -> ranked` (reciprocal rank fusion over vector + Tantivy + BM25 + graph expansion). The three existing modes are unchanged.
- [ ] Failing tests: fixture rankings where the correct item leads one surface and is absent on others → fused ranks it top-3; existing-mode regression tests pass unchanged.
- [ ] Implement RRF + mode plumbing. No neural reranker in this task.

### Task 3.5: Wiki maintenance via consolidation (judge's RC5)
**Files:** Modify: `src/consolidation/transform.rs` (wiki-page outputs), `src/wiki/draft.rs` (update path under lease).
**Interfaces:** Transform gains a wiki-page fate: when Verify/Judge findings touch facts a wiki page states, Transform drafts a page update — written only under a held lease (Task 3.1), through Task 2.9's policy (page updates are interpretive: queue by default), with the same size/abstraction gates as Task 3.3 applied to the updated page. Without this task the wiki is build-once — the stranding Phase 2's dependency note warns against; this task is the mechanism that note promises.
- [ ] Failing tests: a contradicted fact stated on a page → a page-update candidate citing the evidence; the update commits only under lease + policy; an update that pushes the page over its abstraction threshold is refused.
- [ ] Implement the Transform wiki fate + update path.
**Milestone 3 gate:** corpus-QA suite (held-out set included): fused+wiki beats search-only on cross-document questions by ≥ the registered margin, and single-document regression stays within the registered noise band; representation budgets held on all suites; Task 3.5's maintenance path demonstrated on the fixture corpus (a planted fact change propagates to the page through a consolidation pass).

---

## Phase 3 — Curation, cadence, goal profiles (Milestone 4)

### Task 4.1: Hierarchy curator (token-budgeted snapshot)
**Files:** Create: `src/consolidation/curator.rs`; Modify: snapshot path (`handle_get_snapshot`).
**Interfaces:** Snapshot config gains `injected_token_budget`. The curator emits promotion/demotion as consolidation candidates through Task 2.9's policy: promotion requires the persisted verification record from Task 2.9(c) (Review Focus #5); **demotion of guard/safety-class memories is queue-only at any confidence** (Risk R9b). Snapshot responses log which layer (injected vs retrieved) served each item, so derived-layer reliance is measurable (Risk R9d).
- [ ] Failing tests: oversized store → snapshot respects the budget; demoted rule leaves a stub + link; a memory with no verification record is never in the promoted set; a guard-class demotion lands in the queue even at max confidence.
- [ ] Implement curator as a Transform-side producer.
- [ ] **Cold-start note (judge's observation):** verification records exist only on consolidation-committed successors (Task 2.9c), so at Phase 3 start the promotable set is near-empty and the curated snapshot may be far smaller than today's. That is safe by construction, but the Phase 3 report must state the initial promotable-set size and the expected fill rate from consolidation passes, so the shrink is a reported fact, not a surprise.
**Acceptance:** budget enforcement and the verification precondition are test-proven.

### Task 4.2: Staleness scoring + cadence ordering
**Files:** Create: `src/consolidation/staleness.rs`; Modify: `src/consolidation/` pass orchestration; small store addition: `last_accessed` timestamp + per-anchor churn signal (Feasibility F10 — `access_count` exists; the other two inputs don't).
**Interfaces:** `staleness(memory) = f(age, access_count, last_accessed, anchor_churn)`; pass work is ordered by staleness × access; budget exhaustion defers the tail to the next pass **by ordering** (no per-class watermarks — Spec P3-5).
- [ ] Failing tests: high-access + high-churn memory outranks a cold stable one; a truncated pass leaves the tail pending and the watermark honest.
- [ ] Implement scoring + ordering + the store additions.
**Acceptance:** ordering is deterministic and test-proven.

### Task 4.3: Goal profiles (human-authored) + outcome tracking
**Files:** Create: `src/consolidation/profile.rs`; Tests: inline.
**Interfaces:** A per-namespace goal profile — a short human-authored statement of what the namespace's memory is for — consumed by Transform to focus drafting. Outcome tracking records consolidation results against the profile (e.g., fact-capture metrics from membench hooks on the Task 1.4 contract fixture). **There is no instruction-revision loop** (panel kill, §0): profiles change only by human edit.
- [ ] Failing tests: a pass run under a profile produces candidates scoped to the profile's topics; a pass cannot modify its own profile through any path (assert no write path exists).
- [ ] Implement profile storage + consumption + outcome recording.
**Milestone 4 gate:** churn fixture (Task 1.4): cadence-ordered passes detect planted drift at the registered rate; contract fixture: fact capture improves from partial to complete using profile-focused consolidation with a human-authored profile.

---

## Phase 4 — REMOVED (panel adjudication, §0)

Enterprise server mode is not part of this plan. G6 is closed in this plan by the Phase 0 scale note (measured single-node capacity, published). Server mode may return only as its own plan, opened by a named deployment requirement, a threat model, and a tenancy-isolation design — and its audit-log task, if it ever exists, must include the emit-site actor instrumentation the bus currently lacks (Feasibility F7).

---

## Test Plan (whole plan)

- **Unit/integration (Rust, via fork CI):** every task's named tests; discriminating oracles required — a test that passes against the unimplemented code is a failed task. The *test (ubuntu)* check is the iteration signal.
- **Suites (Python, local):** membench learning-loop, coding arm, seeded-error, corpus-QA (+ churn + contract fixtures), each with perfect/no-op discrimination checks; thresholds and fixtures fixed by the Task 1.5 registration commit before any run.
- **Regression:** existing retrieval modes unchanged (Task 3.4 tests); local daemon behavior unchanged (this plan adds no daemon behavior); full existing suite green at every milestone gate.
- **Freeze gate:** from Phase 1 onward, the seeded-error suite runs in fork CI; a regression in correction precision freezes auto-commit (policy table default → queue-only) until fixed (Spec P2-3 / Doc 03 §5).
- **Soaks:** Phase 1 soak on the named copy with independent hand-audit; evidence reported, never asserted.

## Acceptance Criteria (whole plan)

1. Gap mapping, corrected per panel: G1 → M2 + M4; G2 → M3; G3 → M4; G4 → M3; G5 → M1; G6 → M1 scale note, with server mode explicitly descoped pending a named requirement. Every mapping names a mechanical gate.
2. No committed consolidation change lacks a provenance chain to retrievable-or-`Pruned` evidence (audit test, Task 2.9).
3. No destructive delete path is added anywhere; each milestone gate runs a `git grep` check for new delete surface and records the result.
4. All phase reports publish numbers, including negative results — the Done-Funnel A/B and the premise check explicitly included, under the Task 1.5 decision rule.
5. Venue rule honored: zero upstream filings without per-action consent; merges only on Pratik's per-PR word; WIP limit (≤3 program PRs awaiting merge) observed.
6. Mechanical protections live: production-store refusal tests pass (Task 1.0); upstream-push guard installed (Task 0.2); no implementation task's command log shows a live-store touch (reviewer-checked per task).

## Execution notes (SDD)

- One task = one fresh implementer subagent + one fresh reviewer subagent; branches off the M0-resolved base per feature-pr-pipeline; hygiene (scrubbing-public-tree) before PR; CI green + zero unresolved threads before the merge request goes to Pratik.
- Phase starts are separate elections: completing a milestone reports and stops; the next phase begins on Pratik's word. The M0 hard gate and the Milestone 1 premise check are real stop conditions, not formalities.
- Estimated PR load (for the elections): M0 ≈2 PRs; Phase 0 ≈4–5; Phase 1 ≈8–10; Phase 2 ≈3–4; Phase 3 ≈3. CI budget per phase is estimated in Task 0.3's runbook from the measured pilot.
- Authorship separation (Task authorship rule for Phase 0 fixtures/thresholds) is recorded in the registration PR.

## Panel record (2026-11-07, planning-wringer Phase 2)

- **Feasibility Analyst — FEASIBLE-WITH-CHANGES.** P1s: F1 no contradiction judge exists in any base (trait is duplicate-only, `src/judgment/mod.rs:33-42`); F2 bus pulses carry no payload and only Audit/Mesh classes get durable seqs (`src/events/outbox.rs:73-78`, `event.rs:13-19`); F3 no memory→log citation mechanism exists and buffer pruning can delete cited evidence (`src/buffer.rs:182,236-237`); F4 Phase 0 had no runnable backend. P2s: F5 sync target must be `cd66291`, not PR #23 (divergence 772 commits / 154 files); F6 Moved needs the span-hash rule; F7 audit needs emit-site instrumentation (moot — Phase 4 removed); F8 fused tasks split (done: 1.1, 2.5→2.9+2.10, 2.6→2.11, 3.2→3.2+3.3).
- **Spec/Test Architect — SOUND-WITH-CHANGES.** P1s: Transform stage missing (added as Task 2.8; `CandidateChange` gained `id` + `successor_content`); gap-filling task missing (added as Task 2.12; Acceptance #1 corrected); pre-registration step missing (added as Task 1.5). P2s folded in: churn/contract fixtures (Task 1.4), corpus separation test (Task 1.4), policy table + CI freeze gate (Task 2.9 + Test Plan), store-wide sweep scope (Task 2.6), verified-state persistence (Task 2.9c), write-path lease enforcement (Task 3.1), backend interfaces unified (Task 1.1). Phase-4 replication gap (P2-7) moot.
- **Risk Assessor — ACCEPTABLE-WITH-CHANGES.** Required changes all adopted: R1 mechanical guards (Global Constraints + Tasks 0.2/1.0); R2 backend SHA provenance (Task 1.0); R3 hard M0 gate (Task 0.2); R4 audited precision + dual-judge rule (Tasks 2.9/2.11 gate); R5 Phase 4 removed; R6 displacement statement + WIP limit + no-writable-prefix rule (Review Focus #7); R7 A/B pre-registration + public-placement election (Tasks 1.5/1.6); R8 measured runbook + toolchain election at M1 (Task 0.3); R9 rollback-by-pass (Task 2.9d), guard-class queue-only demotion (Task 4.1), queue-burden ceiling (Task 1.5), layer-serving log (Task 4.1); R10 soak copy location + disposal (Task 2.11); R11 premise check (Task 1.6).
- **Devil's Advocate — KILL-PART.** Kills accepted: Phase 4; meta-loop; in-daemon placement. M0 re-cut accepted. Phase 0 anti-gaming accepted. Cannot-kill acknowledged: Phase 0 as a standalone measurement program; Phases 1–2 as evidence-gated future elections. Its Argument 1 (G1 may be a marketing-shaped hole) is answered structurally, not rhetorically: Task 1.3's materiality metric + Task 1.6's premise check can refute Phase 1 before it is elected.
- **Supreme Judge (independent, fresh) — APPROVED-WITH-CHANGES on v1.** Fidelity audit: 7 of 9 adjudication claims fully honored, 2 partial. Five required changes, all applied in v2: **RC1** sync target re-pinned to `8fdae4b` (`cd66291` is PR #56 and does not contain PR #57 — the same defect class the plan faults PR #23 for); **RC2** Task 1.5 registration extended with the materiality floor, wiki size-budget fraction + abstraction threshold, and churn drift-detection rate (three gates consumed numbers the registration never fixed); **RC3** `Create` kind added for gap-filling (2.12's outputs previously had no commit path) + 2.12's gate membership stated; **RC4** §0's "identical on upstream" corrected (same trait surface, different provider internals) + provider identity added to the census; **RC5** Task 3.5 added — the wiki previously had no maintenance path despite Phase 2's own rationale. The judge also caught two breaks both the panel and the author missed (RC3, RC5) and confirmed no residual kill-risk.
